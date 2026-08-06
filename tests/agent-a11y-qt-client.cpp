#include <QApplication>
#include <QLabel>
#include <QPushButton>
#include <QVBoxLayout>
#include <QWidget>

int main(int argc, char **argv) {
  QApplication application(argc, argv);
  QWidget window;
  window.setWindowTitle(QStringLiteral("Agent A11y Qt Fixture"));
  auto *layout = new QVBoxLayout(&window);
  layout->addWidget(
      new QLabel(QStringLiteral("bounded accessibility fixture")));
  layout->addWidget(new QPushButton(QStringLiteral("probe")));
  window.resize(700, 500);
  window.show();
  return application.exec();
}
