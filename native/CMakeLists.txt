cmake_minimum_required(VERSION 3.16)
project(BgPickerVideo LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 6.5 REQUIRED COMPONENTS Core Quick Qml Multimedia)
if(COMMAND qt_policy)
    qt_policy(SET QTP0001 NEW)
endif()

qt_add_qml_module(bgpickervideo
    URI BgPickerVideo
    VERSION 1.0
    PLUGIN_TARGET bgpickervideo
    NO_CACHEGEN
    OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/../BgPickerVideo"
    SOURCES
        VideoFrameSplitter.h
        VideoFrameSplitter.cpp
)

target_link_libraries(bgpickervideo PRIVATE
    Qt6::Core
    Qt6::Quick
    Qt6::Qml
    Qt6::Multimedia
)
