#define _POSIX_C_SOURCE 200809L

#include <X11/ICE/ICElib.h>
#include <X11/SM/SMlib.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <unistd.h>

#define ERROR_BUFFER_SIZE 1024
#define MAX_CONNECTIONS 8

static SmsConn client;
static FILE *events;
static int running = 1;

static void record(const char *event, const char *value) {
    fprintf(events, "%s%s%s\n", event, value == NULL ? "" : "\t",
            value == NULL ? "" : value);
    fflush(events);
}

static void ice_io_error(IceConn connection) {
    (void)connection;
    if (client != NULL) {
        record("ICE_IO_ERROR", NULL);
    }
}

static Status register_client(SmsConn connection, SmPointer data, char *previous_id) {
    (void)data;
    char *generated = NULL;
    char *id = previous_id;
    if (id == NULL) {
        generated = SmsGenerateClientID(connection);
        id = generated;
    }
    if (id == NULL || !SmsRegisterClientReply(connection, id)) {
        free(generated);
        return 0;
    }
    record("REGISTERED", id);
    free(generated);
    return 1;
}

static void interact_request(SmsConn connection, SmPointer data, int dialog_type) {
    (void)data;
    (void)dialog_type;
    SmsInteract(connection);
}

static void interact_done(SmsConn connection, SmPointer data, Bool cancel_shutdown) {
    (void)connection;
    (void)data;
    (void)cancel_shutdown;
}

static void save_yourself_request(SmsConn connection, SmPointer data, int save_type,
                                  Bool shutdown, int interact_style, Bool fast,
                                  Bool global) {
    (void)data;
    (void)global;
    SmsSaveYourself(connection, save_type, shutdown, interact_style, fast);
}

static void save_yourself_phase2_request(SmsConn connection, SmPointer data) {
    (void)data;
    SmsSaveYourselfPhase2(connection);
}

static void save_yourself_done(SmsConn connection, SmPointer data, Bool success) {
    (void)connection;
    (void)data;
    record(success ? "SAVE_DONE" : "SAVE_FAILED", NULL);
}

static void close_connection(SmsConn connection, SmPointer data, int count,
                             char **reasons) {
    (void)data;
    (void)count;
    (void)reasons;
    record("CLOSED", NULL);
    SmsCleanUp(connection);
    client = NULL;
}

static void set_properties(SmsConn connection, SmPointer data, int count,
                           SmProp **properties) {
    (void)connection;
    (void)data;
    for (int index = 0; index < count; ++index) {
        record("PROPERTY", properties[index]->name);
        if (strcmp(properties[index]->name, SmRestartCommand) == 0) {
            for (int value = 0; value + 1 < properties[index]->num_vals; ++value) {
                const char *argument = properties[index]->vals[value].value;
                if (strcmp(argument, "--sm-client-id") == 0) {
                    record("RESTART_ID", properties[index]->vals[value + 1].value);
                }
            }
        }
    }
}

static void delete_properties(SmsConn connection, SmPointer data, int count,
                              char **names) {
    (void)connection;
    (void)data;
    (void)count;
    (void)names;
}

static void get_properties(SmsConn connection, SmPointer data) {
    (void)data;
    SmsReturnProperties(connection, 0, NULL);
}

static Status new_client(SmsConn connection, SmPointer data, unsigned long *mask,
                         SmsCallbacks *callbacks, char **failure_reason) {
    (void)data;
    (void)failure_reason;
    client = connection;
    callbacks->register_client.callback = register_client;
    callbacks->register_client.manager_data = NULL;
    callbacks->interact_request.callback = interact_request;
    callbacks->interact_request.manager_data = NULL;
    callbacks->interact_done.callback = interact_done;
    callbacks->interact_done.manager_data = NULL;
    callbacks->save_yourself_request.callback = save_yourself_request;
    callbacks->save_yourself_request.manager_data = NULL;
    callbacks->save_yourself_phase2_request.callback = save_yourself_phase2_request;
    callbacks->save_yourself_phase2_request.manager_data = NULL;
    callbacks->save_yourself_done.callback = save_yourself_done;
    callbacks->save_yourself_done.manager_data = NULL;
    callbacks->close_connection.callback = close_connection;
    callbacks->close_connection.manager_data = NULL;
    callbacks->set_properties.callback = set_properties;
    callbacks->set_properties.manager_data = NULL;
    callbacks->delete_properties.callback = delete_properties;
    callbacks->delete_properties.manager_data = NULL;
    callbacks->get_properties.callback = get_properties;
    callbacks->get_properties.manager_data = NULL;
    *mask = SmsRegisterClientProcMask | SmsInteractRequestProcMask |
            SmsInteractDoneProcMask | SmsSaveYourselfRequestProcMask |
            SmsSaveYourselfP2RequestProcMask | SmsSaveYourselfDoneProcMask |
            SmsCloseConnectionProcMask | SmsSetPropertiesProcMask |
            SmsDeletePropertiesProcMask | SmsGetPropertiesProcMask;
    return 1;
}

static Bool host_auth(char *host_name) {
    (void)host_name;
    return True;
}

static void handle_command(char *command) {
    command[strcspn(command, "\r\n")] = '\0';
    if (strcmp(command, "SAVE") == 0 && client != NULL) {
        SmsSaveYourself(client, SmSaveBoth, False, SmInteractStyleNone, False);
    } else if (strcmp(command, "COMPLETE") == 0 && client != NULL) {
        SmsSaveComplete(client);
    } else if (strcmp(command, "CANCEL") == 0 && client != NULL) {
        SmsShutdownCancelled(client);
    } else if (strcmp(command, "DIE") == 0 && client != NULL) {
        SmsDie(client);
    } else if (strcmp(command, "QUIT") == 0) {
        running = 0;
    }
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s ADDRESS_FILE EVENT_FILE CONTROL_FIFO\n", argv[0]);
        return 2;
    }
    events = fopen(argv[2], "w");
    if (events == NULL) {
        perror("xsmp-test-manager: event file");
        return 1;
    }
    int control_fd = open(argv[3], O_RDWR | O_NONBLOCK);
    if (control_fd < 0) {
        perror("xsmp-test-manager: control fifo");
        fclose(events);
        return 1;
    }
    FILE *control = fdopen(control_fd, "r");
    if (control == NULL) {
        perror("xsmp-test-manager: control stream");
        close(control_fd);
        fclose(events);
        return 1;
    }
    setvbuf(control, NULL, _IONBF, 0);

    IceSetIOErrorHandler(ice_io_error);
    char error[ERROR_BUFFER_SIZE] = {0};
    if (!SmsInitialize("nobox-test", "1", new_client, NULL, host_auth,
                       (int)sizeof(error), error)) {
        fprintf(stderr, "xsmp-test-manager: SmsInitialize: %s\n", error);
        fclose(control);
        fclose(events);
        return 1;
    }
    int listen_count = 0;
    IceListenObj *listeners = NULL;
    if (!IceListenForConnections(&listen_count, &listeners, (int)sizeof(error), error)) {
        fprintf(stderr, "xsmp-test-manager: listen: %s\n", error);
        fclose(control);
        fclose(events);
        return 1;
    }
    for (int index = 0; index < listen_count; ++index) {
        IceSetHostBasedAuthProc(listeners[index], host_auth);
    }
    char *network_ids = IceComposeNetworkIdList(listen_count, listeners);
    FILE *address = fopen(argv[1], "w");
    if (network_ids == NULL || address == NULL) {
        perror("xsmp-test-manager: address file");
        free(network_ids);
        IceFreeListenObjs(listen_count, listeners);
        fclose(control);
        fclose(events);
        return 1;
    }
    fprintf(address, "%s\n", network_ids);
    fclose(address);
    free(network_ids);

    IceConn connections[MAX_CONNECTIONS] = {0};
    while (running) {
        fd_set reads;
        FD_ZERO(&reads);
        FD_SET(control_fd, &reads);
        int maximum = control_fd;
        for (int index = 0; index < listen_count; ++index) {
            int descriptor = IceGetListenConnectionNumber(listeners[index]);
            FD_SET(descriptor, &reads);
            if (descriptor > maximum) maximum = descriptor;
        }
        for (int index = 0; index < MAX_CONNECTIONS; ++index) {
            if (connections[index] != NULL) {
                int descriptor = IceConnectionNumber(connections[index]);
                FD_SET(descriptor, &reads);
                if (descriptor > maximum) maximum = descriptor;
            }
        }
        int ready = select(maximum + 1, &reads, NULL, NULL, NULL);
        if (ready < 0) {
            if (errno == EINTR) continue;
            perror("xsmp-test-manager: select");
            break;
        }
        for (int index = 0; index < listen_count; ++index) {
            int descriptor = IceGetListenConnectionNumber(listeners[index]);
            if (FD_ISSET(descriptor, &reads)) {
                IceAcceptStatus status;
                IceConn accepted = IceAcceptConnection(listeners[index], &status);
                if (accepted != NULL && status == IceAcceptSuccess) {
                    for (int slot = 0; slot < MAX_CONNECTIONS; ++slot) {
                        if (connections[slot] == NULL) {
                            connections[slot] = accepted;
                            accepted = NULL;
                            break;
                        }
                    }
                    if (accepted != NULL) IceCloseConnection(accepted);
                }
            }
        }
        for (int index = 0; index < MAX_CONNECTIONS; ++index) {
            if (connections[index] != NULL &&
                FD_ISSET(IceConnectionNumber(connections[index]), &reads)) {
                IceProcessMessagesStatus status =
                    IceProcessMessages(connections[index], NULL, NULL);
                if (status != IceProcessMessagesSuccess) {
                    if (status == IceProcessMessagesIOError) {
                        IceCloseConnection(connections[index]);
                    }
                    connections[index] = NULL;
                }
            }
        }
        if (FD_ISSET(control_fd, &reads)) {
            char command[128];
            if (fgets(command, sizeof(command), control) != NULL) {
                handle_command(command);
            } else {
                clearerr(control);
            }
        }
    }

    for (int index = 0; index < MAX_CONNECTIONS; ++index) {
        if (connections[index] != NULL) IceCloseConnection(connections[index]);
    }
    IceFreeListenObjs(listen_count, listeners);
    fclose(control);
    fclose(events);
    return 0;
}
