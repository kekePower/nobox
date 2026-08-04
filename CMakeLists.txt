cmake_minimum_required(VERSION 3.24)

project(nobox LANGUAGES NONE)

find_program(CARGO_EXECUTABLE cargo REQUIRED)
find_program(BASH_EXECUTABLE bash REQUIRED)

set(CARGO_TARGET_DIR "${CMAKE_BINARY_DIR}/cargo")
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

add_custom_command(
    OUTPUT "${NOBOX_EXECUTABLE}"
    COMMAND "${CMAKE_COMMAND}" -E env
            "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
            "${CARGO_EXECUTABLE}" build --locked --package nobox ${CARGO_BUILD_ARGS}
    DEPENDS
        "${CMAKE_SOURCE_DIR}/Cargo.toml"
        "${CMAKE_SOURCE_DIR}/Cargo.lock"
        ${NOBOX_RUST_SOURCES}
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Building nobox (${CARGO_PROFILE})"
    VERBATIM
)
add_custom_target(nobox ALL DEPENDS "${NOBOX_EXECUTABLE}")

add_custom_target(check
    COMMAND "${CMAKE_COMMAND}" -E env "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
            "${CARGO_EXECUTABLE}" fmt --all -- --check
    COMMAND "${CMAKE_COMMAND}" -E env "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
            "${CARGO_EXECUTABLE}" clippy --workspace --all-targets -- -D warnings
    COMMAND "${CMAKE_COMMAND}" -E env "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
            "${CARGO_EXECUTABLE}" test --workspace
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Formatting, linting, and testing the Rust workspace"
    USES_TERMINAL
)

enable_testing()
add_test(
    NAME unit
    COMMAND "${CARGO_EXECUTABLE}" test --workspace
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
)
add_test(
    NAME x11-smoke
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-smoke.sh" "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-smoke PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-frame-smoke
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-frame-smoke.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-frame-smoke PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-workspaces
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-workspaces.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-workspaces PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
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
    NAME x11-applications
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-applications.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-applications PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
add_test(
    NAME x11-focus-cycle
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-focus-cycle.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-focus-cycle PROPERTIES LABELS integration SKIP_RETURN_CODE 77 TIMEOUT 20)
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
    NAME x11-shading
    COMMAND "${BASH_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/tests/x11-shading.sh"
            "${NOBOX_EXECUTABLE}"
)
set_tests_properties(x11-shading PROPERTIES LABELS "integration;openbox"
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

install(PROGRAMS "${NOBOX_EXECUTABLE}" DESTINATION bin)
install(FILES "${CMAKE_SOURCE_DIR}/data/nobox.desktop" DESTINATION share/xsessions)
install(FILES "${CMAKE_SOURCE_DIR}/crates/nobox-config/default.toml"
        DESTINATION share/doc/nobox/examples
        RENAME config.toml)
