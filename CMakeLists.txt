cmake_minimum_required(VERSION 3.24)

project(nobox LANGUAGES NONE)

option(NOBOX_BUILD_PANEL "Build the optional nobox panel" ON)
option(NOBOX_BUILD_AGENT
    "Build and install the optional MCP companion for the agent seat" ON)
option(NOBOX_BUILD_SEMANTIC_HELPER
    "Build and install the optional disposable accessibility helper" ON)

include(CheckLanguage)
check_language(C)
if(CMAKE_C_COMPILER)
    enable_language(C)
endif()
check_language(CXX)
if(CMAKE_CXX_COMPILER)
    enable_language(CXX)
endif()

find_program(CARGO_EXECUTABLE cargo REQUIRED)
get_filename_component(CARGO_BIN_DIR "${CARGO_EXECUTABLE}" DIRECTORY)
find_program(BASH_EXECUTABLE bash REQUIRED)
find_program(PYTHON3_EXECUTABLE python3)
find_program(OPENBOX_EXECUTABLE openbox)
find_package(PkgConfig QUIET)

if(CMAKE_C_COMPILER AND PkgConfig_FOUND)
    pkg_check_modules(XSMP QUIET IMPORTED_TARGET sm ice)
    pkg_check_modules(NOBOX_SETTINGS QUIET libadwaita-1>=1.5 gtk4>=4.10)
endif()
if(CMAKE_CXX_COMPILER AND PkgConfig_FOUND)
    pkg_check_modules(AGENT_A11Y_QT QUIET IMPORTED_TARGET Qt6Widgets)
endif()
if(TARGET PkgConfig::XSMP)
    add_executable(nobox-xsmp tools/nobox-xsmp.c)
    target_compile_features(nobox-xsmp PRIVATE c_std_11)
    target_compile_options(nobox-xsmp PRIVATE -Wall -Wextra -Wpedantic)
    target_link_libraries(nobox-xsmp PRIVATE PkgConfig::XSMP)
else()
    message(STATUS "libSM/libICE development files not found; optional XSMP helper disabled")
endif()
if(TARGET PkgConfig::AGENT_A11Y_QT)
    # Test-only toolkit fixture for the disposable AT-SPI discovery probe.
    add_executable(agent-a11y-qt-client tests/agent-a11y-qt-client.cpp)
    target_compile_features(agent-a11y-qt-client PRIVATE cxx_std_17)
    target_compile_options(agent-a11y-qt-client PRIVATE -Wall -Wextra -Wpedantic)
    target_link_libraries(agent-a11y-qt-client PRIVATE PkgConfig::AGENT_A11Y_QT)
endif()

set(CARGO_TARGET_DIR "${CMAKE_BINARY_DIR}/cargo")
# Cargo resolves rustc, rustdoc, and extension commands such as cargo-fmt from
# PATH even when Cargo itself was selected by absolute path. Keep those tools
# from one installation together when a shell has an unrelated rustup shim
# earlier in PATH.
set(NOBOX_CARGO_ENV
    "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
    "PATH=${CARGO_BIN_DIR}:$ENV{PATH}"
)
if(CMAKE_BUILD_TYPE STREQUAL "Release")
    set(CARGO_BUILD_ARGS --release)
    set(CARGO_PROFILE release)
else()
    set(CARGO_BUILD_ARGS)
    set(CARGO_PROFILE debug)
endif()

set(NOBOX_EXECUTABLE "${CARGO_TARGET_DIR}/${CARGO_PROFILE}/nobox")
file(GLOB_RECURSE NOBOX_RUST_SOURCES CONFIGURE_DEPENDS
    "${CMAKE_SOURCE_DIR}/crates/*.toml"
    "${CMAKE_SOURCE_DIR}/crates/*.rs"
)

# Every shipped Rust binary comes from one cargo invocation. They share a
# target directory, so separate invocations serialize on cargo's own build lock
# anyway while each holds a release build's memory; one command is faster, and
# it cannot half-succeed in a way that leaves `cmake --install` looking for a
# binary that was never built.
set(NOBOX_CARGO_PACKAGES --package nobox)
set(NOBOX_CARGO_OUTPUTS "${NOBOX_EXECUTABLE}")
set(NOBOX_BINARY_SUMMARY nobox)

if(NOBOX_BUILD_PANEL)
    set(NOBOX_PANEL_EXECUTABLE
        "${CARGO_TARGET_DIR}/${CARGO_PROFILE}/nobox-panel")
    list(APPEND NOBOX_CARGO_PACKAGES --package nobox-panel)
    list(APPEND NOBOX_CARGO_OUTPUTS "${NOBOX_PANEL_EXECUTABLE}")
    list(APPEND NOBOX_BINARY_SUMMARY nobox-panel)
endif()

if(NOBOX_BUILD_AGENT)
    set(NOBOX_AGENT_EXECUTABLE "${CARGO_TARGET_DIR}/${CARGO_PROFILE}/nobox-agent")
    list(APPEND NOBOX_CARGO_PACKAGES --package nobox-agent)
    list(APPEND NOBOX_CARGO_OUTPUTS "${NOBOX_AGENT_EXECUTABLE}")
    list(APPEND NOBOX_BINARY_SUMMARY nobox-agent)
else()
    set(NOBOX_AGENT_EXECUTABLE "")
    message(STATUS "The optional MCP companion is disabled; the agent seat "
                   "itself is built into the window manager either way")
endif()

if(NOBOX_BUILD_SEMANTIC_HELPER)
    set(AGENT_SEMANTIC_HELPER_EXECUTABLE
        "${CARGO_TARGET_DIR}/${CARGO_PROFILE}/agent-semantic-helper")
    list(APPEND NOBOX_CARGO_PACKAGES --package agent-semantic-helper)
    list(APPEND NOBOX_CARGO_OUTPUTS "${AGENT_SEMANTIC_HELPER_EXECUTABLE}")
    list(APPEND NOBOX_BINARY_SUMMARY agent-semantic-helper)
else()
    set(AGENT_SEMANTIC_HELPER_EXECUTABLE "")
    message(STATUS "The optional accessibility helper is disabled; semantic "
                   "calls will fail closed without affecting the agent seat")
endif()

if(NOBOX_SETTINGS_FOUND)
    set(NOBOX_SETTINGS_EXECUTABLE
        "${CARGO_TARGET_DIR}/${CARGO_PROFILE}/nobox-settings")
    list(APPEND NOBOX_CARGO_PACKAGES --package nobox-settings
         --features nobox-settings/gui)
    list(APPEND NOBOX_CARGO_OUTPUTS "${NOBOX_SETTINGS_EXECUTABLE}")
    list(APPEND NOBOX_BINARY_SUMMARY nobox-settings)
else()
    message(STATUS "GTK 4/libadwaita development files not found; optional settings app disabled")
endif()

list(JOIN NOBOX_BINARY_SUMMARY ", " NOBOX_BINARY_SUMMARY)
add_custom_command(
    OUTPUT ${NOBOX_CARGO_OUTPUTS}
    COMMAND "${CMAKE_COMMAND}" -E env
            ${NOBOX_CARGO_ENV}
            "${CARGO_EXECUTABLE}" build --locked ${NOBOX_CARGO_PACKAGES}
            ${CARGO_BUILD_ARGS}
    DEPENDS
        "${CMAKE_SOURCE_DIR}/Cargo.toml"
        "${CMAKE_SOURCE_DIR}/Cargo.lock"
        ${NOBOX_RUST_SOURCES}
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Building ${NOBOX_BINARY_SUMMARY} (${CARGO_PROFILE})"
    VERBATIM
)

add_custom_target(nobox ALL DEPENDS "${NOBOX_EXECUTABLE}")
if(NOBOX_BUILD_PANEL)
    add_custom_target(nobox-panel ALL DEPENDS "${NOBOX_PANEL_EXECUTABLE}")
endif()
if(NOBOX_BUILD_AGENT)
    add_custom_target(nobox-agent ALL DEPENDS "${NOBOX_AGENT_EXECUTABLE}")
endif()
if(NOBOX_BUILD_SEMANTIC_HELPER)
    add_custom_target(agent-semantic-helper ALL
        DEPENDS "${AGENT_SEMANTIC_HELPER_EXECUTABLE}")
endif()
if(NOBOX_SETTINGS_FOUND)
    add_custom_target(nobox-settings ALL DEPENDS "${NOBOX_SETTINGS_EXECUTABLE}")
endif()

# The protocol probe is a test fixture rather than a shipped binary, so it is
# built separately - but after the binaries above, never beside them.
set(NOBOX_AGENT_WIRE_PROBE
    "${CARGO_TARGET_DIR}/${CARGO_PROFILE}/examples/nobox-agent-wire-probe")
add_custom_command(
    OUTPUT "${NOBOX_AGENT_WIRE_PROBE}"
    COMMAND "${CMAKE_COMMAND}" -E env
            ${NOBOX_CARGO_ENV}
            "${CARGO_EXECUTABLE}" build --locked --package nobox-agent-wire
            --example nobox-agent-wire-probe ${CARGO_BUILD_ARGS}
    DEPENDS
        ${NOBOX_CARGO_OUTPUTS}
        "${CMAKE_SOURCE_DIR}/Cargo.toml"
        "${CMAKE_SOURCE_DIR}/Cargo.lock"
        ${NOBOX_RUST_SOURCES}
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Building the Nobox agent wire probe (${CARGO_PROFILE})"
    VERBATIM
)
add_custom_target(nobox-agent-wire-probe ALL DEPENDS "${NOBOX_AGENT_WIRE_PROBE}")

add_custom_target(check
    COMMAND "${CMAKE_COMMAND}" -E env ${NOBOX_CARGO_ENV}
            "${CARGO_EXECUTABLE}" fmt --all -- --check
    COMMAND "${CMAKE_COMMAND}" -E env ${NOBOX_CARGO_ENV}
            "${CARGO_EXECUTABLE}" clippy --workspace --all-targets -- -D warnings
    COMMAND "${CMAKE_COMMAND}" -E env ${NOBOX_CARGO_ENV}
            "${CARGO_EXECUTABLE}" test --workspace
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Formatting, linting, and testing the Rust workspace"
    USES_TERMINAL
)
if(NOBOX_SETTINGS_FOUND)
    add_custom_target(check-settings
        COMMAND "${CMAKE_COMMAND}" -E env ${NOBOX_CARGO_ENV}
                "${CARGO_EXECUTABLE}" clippy --package nobox-settings
                --features gui --all-targets -- -D warnings
        COMMAND "${CMAKE_COMMAND}" -E env ${NOBOX_CARGO_ENV}
                "${CARGO_EXECUTABLE}" test --package nobox-settings
                --features gui --all-targets
        WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
        COMMENT "Linting and testing the optional native settings application"
        USES_TERMINAL
    )
    add_dependencies(check check-settings)
endif()

enable_testing()
if(PYTHON3_EXECUTABLE)
    add_test(
        NAME agent-a11y-probe-fixtures
        COMMAND "${PYTHON3_EXECUTABLE}"
                "${CMAKE_SOURCE_DIR}/tests/agent-a11y-probe-fixtures.py"
                "${CMAKE_SOURCE_DIR}/tools/agent-a11y-probe.py"
    )
    set_tests_properties(agent-a11y-probe-fixtures PROPERTIES
        LABELS "unit;agent" TIMEOUT 5)
    if(TARGET agent-semantic-helper)
        add_test(
            NAME agent-semantic-helper-fixtures
            COMMAND "${PYTHON3_EXECUTABLE}"
                    "${CMAKE_SOURCE_DIR}/tests/agent-semantic-helper-fixtures.py"
                    "${AGENT_SEMANTIC_HELPER_EXECUTABLE}"
        )
        set_tests_properties(agent-semantic-helper-fixtures PROPERTIES
            LABELS "unit;agent" TIMEOUT 5)
    endif()
else()
    message(STATUS "python3 not found; AT-SPI probe fixture test disabled")
endif()
add_test(
    NAME unit
    COMMAND "${CMAKE_COMMAND}" -E env ${NOBOX_CARGO_ENV}
            "${CARGO_EXECUTABLE}" test --workspace
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
)
add_test(
    NAME cli-theme-import
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/cli-theme-import.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(cli-theme-import PROPERTIES LABELS "cli;openbox" TIMEOUT 10)
if(TARGET nobox-settings)
    add_test(
        NAME x11-settings
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-settings.sh"
                "${NOBOX_EXECUTABLE}" "${NOBOX_SETTINGS_EXECUTABLE}"
                "${CMAKE_SOURCE_DIR}/crates/nobox-config/default.toml"
    )
    set_tests_properties(x11-settings PROPERTIES LABELS integration
                         SKIP_RETURN_CODE 77 TIMEOUT 15)
endif()
if(TARGET nobox-panel)
    add_test(
        NAME x11-panel
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-panel.sh"
                "${NOBOX_EXECUTABLE}"
    )
    set_tests_properties(x11-panel PROPERTIES LABELS integration
                         SKIP_RETURN_CODE 77 TIMEOUT 20)
endif()
if(TARGET nobox-xsmp)
    add_test(
        NAME x11-xsmp
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-xsmp.sh"
                "${NOBOX_EXECUTABLE}" "$<TARGET_FILE:nobox-xsmp>"
    )
    set_tests_properties(x11-xsmp PROPERTIES LABELS "integration;openbox"
                         SKIP_RETURN_CODE 77 TIMEOUT 20)
endif()
add_test(
    NAME x11-doctor
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-doctor.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-doctor PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 15)
add_test(
    NAME x11-smoke
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-smoke.sh" "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-smoke PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-agent-seat
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-agent-seat.sh"
            "${NOBOX_EXECUTABLE}" "${NOBOX_AGENT_WIRE_PROBE}" "${NOBOX_AGENT_EXECUTABLE}"
            "${CMAKE_SOURCE_DIR}/tests/agent-semantic-helper-fault.py"
)
set_tests_properties(x11-agent-seat PROPERTIES LABELS "integration;agent"
    SKIP_RETURN_CODE 77 TIMEOUT 90)
set(AGENT_A11Y_HELPER_FOR_TEST "${CMAKE_SOURCE_DIR}/tools/agent-a11y-probe.py")
if(TARGET agent-semantic-helper)
    set(AGENT_A11Y_HELPER_FOR_TEST "${AGENT_SEMANTIC_HELPER_EXECUTABLE}")
endif()
set(AGENT_A11Y_PROBE_TEST_ARGS
    "${NOBOX_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tools/agent-a11y-probe.py"
    "${AGENT_A11Y_HELPER_FOR_TEST}" "${NOBOX_AGENT_WIRE_PROBE}")
if(TARGET agent-a11y-qt-client)
    list(APPEND AGENT_A11Y_PROBE_TEST_ARGS "$<TARGET_FILE:agent-a11y-qt-client>")
endif()
add_test(
    NAME x11-agent-a11y-probe
    COMMAND "${BASH_EXECUTABLE}"
            "${CMAKE_SOURCE_DIR}/tests/x11-agent-a11y-probe.sh"
            ${AGENT_A11Y_PROBE_TEST_ARGS}
)
set_tests_properties(x11-agent-a11y-probe PROPERTIES LABELS "integration;agent"
    SKIP_RETURN_CODE 77 TIMEOUT 60)
find_program(AGENT_A11Y_FIREFOX_BROWSER NAMES zen zen-browser firefox)
if(AGENT_A11Y_FIREFOX_BROWSER AND TARGET agent-semantic-helper)
    add_test(
        NAME x11-agent-browser-a11y
        COMMAND "${BASH_EXECUTABLE}"
                "${CMAKE_SOURCE_DIR}/tests/x11-agent-browser-a11y.sh"
                "${NOBOX_EXECUTABLE}" "${NOBOX_AGENT_WIRE_PROBE}"
                "${AGENT_A11Y_FIREFOX_BROWSER}"
                "${CMAKE_SOURCE_DIR}/tests/agent-semantic-video.html"
    )
    set_tests_properties(x11-agent-browser-a11y PROPERTIES
        LABELS "integration;agent" SKIP_RETURN_CODE 77 TIMEOUT 60)
endif()
find_program(AGENT_A11Y_CHROMIUM_BROWSER
    NAMES chromium chromium-browser google-chrome google-chrome-beta)
if(AGENT_A11Y_CHROMIUM_BROWSER AND TARGET agent-semantic-helper)
    add_test(
        NAME x11-agent-chromium-a11y
        COMMAND "${BASH_EXECUTABLE}"
                "${CMAKE_SOURCE_DIR}/tests/x11-agent-browser-a11y.sh"
                "${NOBOX_EXECUTABLE}" "${NOBOX_AGENT_WIRE_PROBE}"
                "${AGENT_A11Y_CHROMIUM_BROWSER}"
                "${CMAKE_SOURCE_DIR}/tests/agent-semantic-video.html"
                chromium
    )
    set_tests_properties(x11-agent-chromium-a11y PROPERTIES
        LABELS "integration;agent" SKIP_RETURN_CODE 77 TIMEOUT 60)
endif()
if(TARGET nobox-agent)
    add_test(
        NAME x11-agent-mcp
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-agent-mcp.sh"
                "${NOBOX_EXECUTABLE}" "${NOBOX_AGENT_EXECUTABLE}"
    )
    set_tests_properties(x11-agent-mcp PROPERTIES LABELS "integration;agent"
        SKIP_RETURN_CODE 77 TIMEOUT 90)
endif()
add_test(
    NAME x11-frame-smoke
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-frame-smoke.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-frame-smoke PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-font-fallback
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-font-fallback.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-font-fallback PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-workspaces
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-workspaces.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-workspaces PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-workspace-actions
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-workspace-actions.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-workspace-actions PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-execute
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-execute.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-execute PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-exit
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-exit.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-exit PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-restart
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-restart.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-restart PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-conditional-actions
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-conditional-actions.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-conditional-actions PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-show-desktop
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-show-desktop.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-show-desktop PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-mouse-bindings
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-mouse-bindings.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-mouse-bindings PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-window-snapping
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-window-snapping.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-window-snapping PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-applications
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-applications.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-applications PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-opacity
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-opacity.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-opacity PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-margins
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-margins.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-margins PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-focus-cycle
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-focus-cycle.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-focus-cycle PROPERTIES LABELS "integration;openbox" SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-focus-stealing
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-focus-stealing.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-focus-stealing PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-focus-policy
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-focus-policy.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-focus-policy PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-focus-events
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-focus-events.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-focus-events PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-colormaps
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-colormaps.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-colormaps PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-input-cursor
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-input-cursor.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-input-cursor PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-ping
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-ping.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-ping PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-sync-resize
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-sync-resize.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-sync-resize PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 25)
add_test(
    NAME x11-net-moveresize
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-net-moveresize.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-net-moveresize PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-selections
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-selections.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-selections PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-session-restore
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-session-restore.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-session-restore PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-menus
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-menus.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-menus PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-placement
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-placement.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-placement PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-presentation
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-presentation.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-presentation PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-geometry-actions
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-geometry-actions.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-geometry-actions PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-icons
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-icons.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-icons PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-edge-compat
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-edge-compat.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-edge-compat PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-configure-notify
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-configure-notify.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-configure-notify PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-shading
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-shading.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-shading PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-stacking-actions
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-stacking-actions.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-stacking-actions PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-explicit-state-actions
    COMMAND "${BASH_EXECUTABLE}"
            "${CMAKE_SOURCE_DIR}/tests/x11-explicit-state-actions.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-explicit-state-actions PROPERTIES LABELS "integration;openbox"
                     SKIP_RETURN_CODE 77 TIMEOUT 20)
if(EXISTS "${CMAKE_SOURCE_DIR}/../openbox/tests/aspect.c")
    add_test(
        NAME x11-shape
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-shape.sh"
                "${NOBOX_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/../openbox"
    )
    set_tests_properties(x11-shape PROPERTIES LABELS "integration;openbox"
                         SKIP_RETURN_CODE 77 TIMEOUT 20)
    add_test(
        NAME x11-legacy-fullscreen
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-legacy-fullscreen.sh"
                "${NOBOX_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/../openbox"
    )
    set_tests_properties(x11-legacy-fullscreen PROPERTIES LABELS "integration;openbox"
                         SKIP_RETURN_CODE 77 TIMEOUT 20)
    add_test(
        NAME x11-size-compat
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-size-compat.sh"
                "${NOBOX_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/../openbox"
    )
    set_tests_properties(x11-size-compat PROPERTIES LABELS "integration;openbox"
                         SKIP_RETURN_CODE 77 TIMEOUT 20)
    add_test(
        NAME x11-group-transients
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-group-transients.sh"
                "${NOBOX_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/../openbox"
    )
    set_tests_properties(x11-group-transients PROPERTIES LABELS "integration;openbox"
                         SKIP_RETURN_CODE 77 TIMEOUT 20)
    add_test(
        NAME openbox-regressions
        COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/openbox-regressions.sh"
                "${NOBOX_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/../openbox"
    )
    set_tests_properties(openbox-regressions PROPERTIES LABELS "integration;openbox" TIMEOUT 20)
endif()

add_custom_target(test-all
    COMMAND "${CMAKE_CTEST_COMMAND}" --output-on-failure
    DEPENDS nobox
    WORKING_DIRECTORY "${CMAKE_BINARY_DIR}"
    COMMENT "Building nobox and running the complete test suite"
    USES_TERMINAL
)
if(TARGET nobox-xsmp)
    add_dependencies(test-all nobox-xsmp)
endif()
if(TARGET nobox-settings)
    add_dependencies(test-all nobox-settings)
endif()
if(TARGET nobox-panel)
    add_dependencies(test-all nobox-panel)
endif()

set(NOBOX_PERFORMANCE_RUNS 5 CACHE STRING "Repeated runs in the performance report")
set(NOBOX_PERFORMANCE_CLIENTS 50 CACHE STRING "Managed clients in the performance report")
set(NOBOX_PERFORMANCE_WORKLOAD smart CACHE STRING "Performance workload: smart or positioned")
add_custom_target(performance-report
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tools/performance-report.sh"
            "${NOBOX_EXECUTABLE}" "${OPENBOX_EXECUTABLE}"
            "${NOBOX_PERFORMANCE_RUNS}" "${NOBOX_PERFORMANCE_CLIENTS}"
            "${NOBOX_PERFORMANCE_WORKLOAD}"
    DEPENDS nobox
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Comparing nobox and Openbox on isolated nested X servers"
    USES_TERMINAL
)

# `cmake --install` only copies an existing build tree, and some build presets
# build one target rather than everything. Naming the missing binary and the
# command that produces it beats a bare "cannot find file" pointing inside the
# build directory.
set(NOBOX_INSTALLED_BINARIES ${NOBOX_CARGO_OUTPUTS})
if(TARGET nobox-xsmp)
    list(APPEND NOBOX_INSTALLED_BINARIES "$<TARGET_FILE:nobox-xsmp>")
endif()
install(CODE "
foreach(binary ${NOBOX_INSTALLED_BINARIES})
    if(NOT EXISTS \"\${binary}\")
        message(FATAL_ERROR
            \"\${binary} has not been built.\\n\"
            \"Run: cmake --build \\\"${CMAKE_BINARY_DIR}\\\"\\n\"
            \"The performance preset builds only its benchmark, not \"
            \"everything this install needs.\")
    endif()
endforeach()
")
install(PROGRAMS "${NOBOX_EXECUTABLE}" DESTINATION bin)
if(TARGET nobox-panel)
    install(PROGRAMS "${NOBOX_PANEL_EXECUTABLE}" DESTINATION bin)
endif()
if(TARGET nobox-xsmp)
    install(TARGETS nobox-xsmp RUNTIME DESTINATION bin)
endif()
if(TARGET nobox-agent)
    install(PROGRAMS "${NOBOX_AGENT_EXECUTABLE}" DESTINATION bin)
    install(FILES "${CMAKE_SOURCE_DIR}/crates/nobox-agent/README.md"
            DESTINATION share/doc/nobox
            RENAME nobox-agent.md)
endif()
if(TARGET agent-semantic-helper)
    install(PROGRAMS "${AGENT_SEMANTIC_HELPER_EXECUTABLE}"
            DESTINATION libexec/nobox)
endif()
if(TARGET nobox-settings)
    install(PROGRAMS "${NOBOX_SETTINGS_EXECUTABLE}" DESTINATION bin)
    install(FILES "${CMAKE_SOURCE_DIR}/data/nobox-settings.desktop"
            DESTINATION share/applications)
endif()
install(FILES "${CMAKE_SOURCE_DIR}/data/nobox.desktop" DESTINATION share/xsessions)
install(FILES "${CMAKE_SOURCE_DIR}/crates/nobox-config/default.toml"
        DESTINATION share/doc/nobox/examples
        RENAME config.toml)
