#define _POSIX_C_SOURCE 200809L

#include <X11/ICE/ICElib.h>
#include <X11/SM/SMlib.h>
#include <ctype.h>
#include <errno.h>
#include <pwd.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/types.h>
#include <unistd.h>

#define ERROR_BUFFER_SIZE 1024
#define MAX_CLIENT_ID_BYTES 1024
#define MAX_COMMAND_BYTES 256

static SmcConn sm_connection;
static int save_pending;
static int running = 1;

static int valid_client_id(const char *client_id) {
    if (client_id == NULL || client_id[0] == '\0' ||
        strlen(client_id) > MAX_CLIENT_ID_BYTES) {
        return 0;
    }
    for (const unsigned char *byte = (const unsigned char *)client_id; *byte != '\0'; ++byte) {
        if (iscntrl(*byte)) return 0;
    }
    return 1;
}

static void emit_event(const char *event) {
    if (printf("%s\n", event) < 0 || fflush(stdout) != 0) {
        running = 0;
    }
}

static void save_yourself(SmcConn connection, SmPointer data, int save_type,
                          Bool shutdown, int interact_style, Bool fast) {
    (void)data;
    (void)shutdown;
    (void)interact_style;
    (void)fast;
    if (save_type == SmSaveGlobal) {
        SmcSaveYourselfDone(connection, True);
    } else if (!save_pending) {
        save_pending = 1;
        emit_event("SAVE");
    }
}

static void die(SmcConn connection, SmPointer data) {
    (void)connection;
    (void)data;
    emit_event("DIE");
}

static void save_complete(SmcConn connection, SmPointer data) {
    (void)connection;
    (void)data;
    emit_event("SAVE_COMPLETE");
}

static void shutdown_cancelled(SmcConn connection, SmPointer data) {
    (void)connection;
    (void)data;
    emit_event("SHUTDOWN_CANCELLED");
}

static void set_array_property(const char *name, const char *value) {
    SmPropValue property_value = {
        .length = (int)strlen(value) + 1,
        .value = (SmPointer)value,
    };
    SmProp property = {
        .name = (char *)name,
        .type = SmARRAY8,
        .num_vals = 1,
        .vals = &property_value,
    };
    SmProp *properties[] = {&property};
    SmcSetProperties(sm_connection, 1, properties);
}

static void set_card_property(const char *name, unsigned char value) {
    SmPropValue property_value = {
        .length = 1,
        .value = &value,
    };
    SmProp property = {
        .name = (char *)name,
        .type = SmCARD8,
        .num_vals = 1,
        .vals = &property_value,
    };
    SmProp *properties[] = {&property};
    SmcSetProperties(sm_connection, 1, properties);
}

static void set_command_property(const char *name, int count, char **values) {
    SmPropValue *property_values = calloc((size_t)count, sizeof(*property_values));
    if (property_values == NULL) {
        fprintf(stderr, "nobox-xsmp: could not allocate command property\n");
        return;
    }
    for (int index = 0; index < count; ++index) {
        property_values[index].length = (int)strlen(values[index]) + 1;
        property_values[index].value = values[index];
    }
    SmProp property = {
        .name = (char *)name,
        .type = SmLISTofARRAY8,
        .num_vals = count,
        .vals = property_values,
    };
    SmProp *properties[] = {&property};
    SmcSetProperties(sm_connection, 1, properties);
    free(property_values);
}

static void publish_properties(int command_count, char **command, const char *client_id) {
    char pid[32];
    snprintf(pid, sizeof(pid), "%ld", (long)getpid());
    set_array_property(SmProgram, command[0]);
    set_array_property(SmProcessID, pid);
    struct passwd *user = getpwuid(getuid());
    if (user != NULL && user->pw_name != NULL) {
        set_array_property(SmUserID, user->pw_name);
    }
    set_card_property(SmRestartStyleHint, SmRestartImmediately);
    set_card_property("_GSM_Priority", 20);
    set_command_property(SmCloneCommand, command_count, command);

    char **restart = calloc((size_t)command_count + 2, sizeof(*restart));
    if (restart == NULL) {
        fprintf(stderr, "nobox-xsmp: could not allocate restart command\n");
        return;
    }
    for (int index = 0; index < command_count; ++index) {
        restart[index] = command[index];
    }
    restart[command_count] = "--sm-client-id";
    restart[command_count + 1] = (char *)client_id;
    set_command_property(SmRestartCommand, command_count + 2, restart);
    free(restart);
}

static void handle_command(char *command) {
    command[strcspn(command, "\r\n")] = '\0';
    if (strcmp(command, "SAVE_DONE\t1") == 0 || strcmp(command, "SAVE_DONE\t0") == 0) {
        if (save_pending) {
            SmcSaveYourselfDone(sm_connection, command[10] == '1');
            save_pending = 0;
        }
    } else if (strcmp(command, "CLOSE\t1") == 0 || strcmp(command, "CLOSE\t0") == 0) {
        if (command[6] == '1') {
            set_card_property(SmRestartStyleHint, SmRestartIfRunning);
        }
        running = 0;
    } else if (command[0] != '\0') {
        fprintf(stderr, "nobox-xsmp: ignored unknown command\n");
    }
}

static int event_loop(void) {
    IceConn ice = SmcGetIceConnection(sm_connection);
    int ice_fd = IceConnectionNumber(ice);
    while (running) {
        fd_set reads;
        FD_ZERO(&reads);
        FD_SET(ice_fd, &reads);
        FD_SET(STDIN_FILENO, &reads);
        int maximum = ice_fd > STDIN_FILENO ? ice_fd : STDIN_FILENO;
        int ready = select(maximum + 1, &reads, NULL, NULL, NULL);
        if (ready < 0) {
            if (errno == EINTR) {
                continue;
            }
            perror("nobox-xsmp: select");
            return 1;
        }
        if (FD_ISSET(ice_fd, &reads)) {
            IceProcessMessagesStatus status = IceProcessMessages(ice, NULL, NULL);
            if (status != IceProcessMessagesSuccess) {
                running = 0;
            }
        }
        if (running && FD_ISSET(STDIN_FILENO, &reads)) {
            char command[MAX_COMMAND_BYTES];
            if (fgets(command, sizeof(command), stdin) == NULL) {
                running = 0;
            } else {
                handle_command(command);
            }
        }
    }
    return 0;
}

int main(int argc, char **argv) {
    const char *requested_id = NULL;
    int argument = 1;
    if (argument + 1 < argc && strcmp(argv[argument], "--client-id") == 0) {
        requested_id = argv[argument + 1];
        argument += 2;
        if (!valid_client_id(requested_id)) {
            fputs("nobox-xsmp: invalid client id\n", stderr);
            return 2;
        }
    }
    if (argument >= argc || strcmp(argv[argument], "--") != 0 || argument + 1 >= argc) {
        fputs("usage: nobox-xsmp [--client-id ID] -- NOBOX [ARG...]\n", stderr);
        return 2;
    }
    ++argument;

    signal(SIGPIPE, SIG_IGN);
    SmcCallbacks callbacks = {
        .save_yourself = {.callback = save_yourself, .client_data = NULL},
        .die = {.callback = die, .client_data = NULL},
        .save_complete = {.callback = save_complete, .client_data = NULL},
        .shutdown_cancelled = {.callback = shutdown_cancelled, .client_data = NULL},
    };
    char error[ERROR_BUFFER_SIZE] = {0};
    char *client_id = NULL;
    sm_connection = SmcOpenConnection(
        NULL, NULL, 1, 0,
        SmcSaveYourselfProcMask | SmcDieProcMask | SmcSaveCompleteProcMask |
            SmcShutdownCancelledProcMask,
        &callbacks, requested_id, &client_id, (int)sizeof(error), error);
    if (sm_connection == NULL) {
        fprintf(stderr, "nobox-xsmp: could not connect: %s\n", error);
        return 1;
    }
    if (!valid_client_id(client_id)) {
        fputs("nobox-xsmp: session manager returned an invalid client id\n", stderr);
        SmcCloseConnection(sm_connection, 0, NULL);
        free(client_id);
        return 1;
    }

    publish_properties(argc - argument, &argv[argument], client_id);
    if (printf("CONNECTED\t%s\n", client_id) < 0 || fflush(stdout) != 0) {
        SmcCloseConnection(sm_connection, 0, NULL);
        free(client_id);
        return 1;
    }
    int result = event_loop();
    SmcCloseConnection(sm_connection, 0, NULL);
    free(client_id);
    return result;
}
