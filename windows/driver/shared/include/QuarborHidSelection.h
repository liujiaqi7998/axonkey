#pragma once
#include <stddef.h>

/* Bus-independent selection helpers, shared by kernel code and unit tests. */
static __inline wchar_t QuarborUpper(wchar_t value)
{
    return value >= L'a' && value <= L'z' ? value - (L'a' - L'A') : value;
}

static __inline int QuarborEqual(const wchar_t* left, const wchar_t* right)
{
    while (*left && *right && QuarborUpper(*left) == QuarborUpper(*right)) { ++left; ++right; }
    return *left == *right;
}

static __inline int QuarborIsHidKeyboard(const wchar_t* classGuid, const wchar_t* enumerator)
{
    return QuarborEqual(classGuid, L"{4D36E96B-E325-11CE-BFC1-08002BE10318}") &&
        QuarborEqual(enumerator, L"HID");
}

static __inline unsigned long QuarborCollectionNumber(const wchar_t* id)
{
    for (; *id; ++id) {
        if (*id == L'&' && QuarborUpper(id[1]) == L'C' &&
            QuarborUpper(id[2]) == L'O' && QuarborUpper(id[3]) == L'L') {
            unsigned long value = 0;
            for (unsigned int i = 4; i < 6; ++i) {
                const wchar_t digit = QuarborUpper(id[i]);
                if (digit >= L'0' && digit <= L'9') value = value * 16 + digit - L'0';
                else if (digit >= L'A' && digit <= L'F') value = value * 16 + digit - L'A' + 10;
                else return 0;
            }
            return value;
        }
    }
    return 0;
}

/* PnP properties are byte-counted buffers. Do not assume the registry/bus
 * supplied a terminator, including when an &COL token straddles the end. */
static __inline unsigned long QuarborCollectionNumberBounded(const wchar_t* id, size_t characters)
{
    if (id == NULL) return 0;
    size_t length = 0;
    while (length < characters && id[length] != L'\0') ++length;
    if (length == characters) return 0;
    return QuarborCollectionNumber(id);
}
