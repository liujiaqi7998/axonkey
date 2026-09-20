// Instantiate the HID interface GUID in an isolated translation unit. Keeping
// INITGUID out of main.cpp prevents headers for HID and virtual-microphone I/O
// from defining the Windows SDK's winioctl.h GUIDs more than once.
#include <windows.h>
#include <initguid.h>
#include <QuarborHidFilter.h>
