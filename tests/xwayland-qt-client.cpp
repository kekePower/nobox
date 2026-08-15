#include <QApplication>
#include <QTimer>
#include <QWidget>
#include <cstdio>

int main(int argc, char **argv) {
    QApplication application(argc, argv);
    QWidget window;
    window.setObjectName("nobox-xwayland-qt");
    window.setWindowTitle("nobox XWayland Qt client");
    window.resize(240, 140);
    window.show();
    application.processEvents();
    std::printf("window=0x%lx\n", static_cast<unsigned long>(window.winId()));
    std::fflush(stdout);

    bool was_active = false;
    QTimer timer;
    QObject::connect(&timer, &QTimer::timeout, [&window, &was_active]() {
        bool active = window.isActiveWindow();
        if (active && !was_active) {
            std::puts("focus=qt");
            std::fflush(stdout);
        }
        was_active = active;
    });
    timer.start(10);
    return application.exec();
}
