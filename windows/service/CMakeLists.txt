cmake_minimum_required(VERSION 3.20)
project(AxonkeyService LANGUAGES CXX)

# Pin spdlog v1.15.3 to an immutable commit for reproducible builds.
include(FetchContent)
set(SPDLOG_WCHAR_FILENAMES ON CACHE BOOL "Support Unicode service paths" FORCE)
FetchContent_Declare(spdlog
  GIT_REPOSITORY https://github.com/gabime/spdlog.git
  GIT_TAG 6fa36017cfd5731d617e1a934f0e5ea9c4445b13
  GIT_SUBMODULES "")
FetchContent_MakeAvailable(spdlog)

add_library(ServiceLogging STATIC ServiceLog.cpp)
target_compile_features(ServiceLogging PRIVATE cxx_std_20)
target_compile_definitions(ServiceLogging PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_link_libraries(ServiceLogging PRIVATE spdlog::spdlog)
if(MSVC)
  target_compile_options(ServiceLogging PRIVATE /W4 /permissive- /utf-8)
endif()

add_executable(AxonkeyService main.cpp HidInterfaceGuid.cpp ServiceConfig.cpp VoiceReceiver.cpp RpcServer.cpp
    VoiceAudioSession.cpp VirtualMicrophoneSink.cpp
    ../../protobuf/axonkey_rpc.cpp
    ../driver/shared/src/QuarborDeviceSetup.cpp)
# C++/WinRT uses standard coroutines in C++20. C++17 selects the experimental
# coroutine headers, which recent MSVC versions reject as deprecated.
target_compile_features(AxonkeyService PRIVATE cxx_std_20)
target_compile_definitions(AxonkeyService PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(AxonkeyService PRIVATE ../driver/shared/include ../../protobuf)
target_link_libraries(AxonkeyService PRIVATE Setupapi Cfgmgr32 Advapi32 ServiceLogging)

# Keep service artifacts next to the sources so the desktop packaging step can
# consume a stable directory without knowing the CMake build directory.
set(SERVICE_DIST_DIR "${CMAKE_CURRENT_SOURCE_DIR}/dist")
set_target_properties(ServiceLogging AxonkeyService PROPERTIES
  RUNTIME_OUTPUT_DIRECTORY "${SERVICE_DIST_DIR}"
  LIBRARY_OUTPUT_DIRECTORY "${SERVICE_DIST_DIR}"
  ARCHIVE_OUTPUT_DIRECTORY "${SERVICE_DIST_DIR}")
if(MSVC)
  set_target_properties(AxonkeyService PROPERTIES
    PDB_OUTPUT_DIRECTORY "${SERVICE_DIST_DIR}"
    COMPILE_PDB_OUTPUT_DIRECTORY "${SERVICE_DIST_DIR}")
endif()

file(GLOB CPPWINRT_INCLUDE_DIRS
  "C:/Program Files (x86)/Windows Kits/10/Include/*/cppwinrt")
list(SORT CPPWINRT_INCLUDE_DIRS ORDER DESCENDING)
if(CPPWINRT_INCLUDE_DIRS)
  list(GET CPPWINRT_INCLUDE_DIRS 0 CPPWINRT_INCLUDE_DIR)
  target_include_directories(AxonkeyService PRIVATE "${CPPWINRT_INCLUDE_DIR}")
endif()
target_link_libraries(AxonkeyService PRIVATE windowsapp ole32 shell32)

# Keep symbols in the optimized service so future Windows crash offsets can
# be resolved without having to reproduce the failure on a debug build.
if(MSVC)
  target_compile_options(AxonkeyService PRIVATE /Zi)
  target_link_options(AxonkeyService PRIVATE /DEBUG)
endif()

# MinGW's SetupAPI headers omit these class-property constants although the
# Windows SDK/WDK defines them. Keep the shared driver helper portable.
if(CMAKE_CXX_COMPILER_ID STREQUAL "GNU")
  target_compile_definitions(AxonkeyService PRIVATE
    SPCRP_UPPERFILTERS=0x00000011 SPCRP_LOWERFILTERS=0x00000012)
  target_link_options(AxonkeyService PRIVATE -municode)
endif()

if(MSVC)
  target_compile_options(AxonkeyService PRIVATE /W4 /permissive- /utf-8)
endif()
