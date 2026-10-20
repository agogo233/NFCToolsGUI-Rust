/*-
 * Free/Libre Near Field Communication (NFC) library
 *
 * Libnfc historical contributors:
 * Copyright (C) 2009      Roel Verdult
 * Copyright (C) 2009-2013 Romuald Conty
 * Copyright (C) 2010-2012 Romain Tartière
 * Copyright (C) 2010-2013 Philippe Teuwen
 * Copyright (C) 2012-2013 Ludovic Rousseau
 * See AUTHORS file for a more comprehensive list of contributors.
 * Additional contributors of this file:
 * Copyright (C) 2011      Adam Laurie
 * Copyright (C) 2014      Dario Carluccio
 * Copyright (C) 2021      Duama
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *  1) Redistributions of source code must retain the above copyright notice,
 *     this list of conditions and the following disclaimer.
 *  2 )Redistributions in binary form must reproduce the above copyright
 *     notice, this list of conditions and the following disclaimer in the
 *     documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
 * AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 * IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
 * ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
 * LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
 * CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
 * SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
 * INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
 * CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
 * ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
 * POSSIBILITY OF SUCH DAMAGE.
 *
 * Note that this license only applies on the examples, NFC library itself is under LGPL
 *
 */

/**
 * @file nfc-mfsetuid.c
 * @brief Set block0 (UID) of special Mifare cards (aka UFUID)
 */

/**
 * based on nfc-mflock.c
 */

#ifdef HAVE_CONFIG_H
#  include "config.h"
#endif // HAVE_CONFIG_H

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <stddef.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>

#include <nfc/nfc.h>

#define MAX_FRAME_LEN 264
#define BLOCK0_LEN 16
#define BLOCK0_HEX_LEN 32
#define BCC_INDEX 4
#define TRANSCEIVE_TIMEOUT_MS 1000
#define CASCADE_BIT 0x04

static uint8_t abtRx[MAX_FRAME_LEN];
static uint8_t abtUid[12];
static uint8_t abtNewBlock0[BLOCK0_LEN];
static uint8_t abtReadBlock0[BLOCK0_LEN];

static int szRxBits;
static int szRxBytes;

static bool quiet_output = false;
static bool force_bcc = false;
static bool skip_verify = false;

static nfc_device *pnd;

// ISO14443A Anti-Collision Commands
static uint8_t abtReqa[1] = { 0x26 };
static uint8_t abtSelectAll[2] = { 0x93, 0x20 };
static uint8_t abtSelectTag[9] = { 0x93, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 };
static uint8_t abtHalt[4] = { 0x50, 0x00, 0x00, 0x00 };

// Gen1A magic backdoor commands
static uint8_t abtUnlock1[1] = { 0x40 };
static uint8_t abtUnlock2[1] = { 0x43 };
static uint8_t abtWriteBlock0Cmd[4] = { 0xa0, 0x00, 0x00, 0x00 };
static uint8_t abtReadBlock0Cmd[4] = { 0x30, 0x00, 0x00, 0x00 };

static void
print_hex(const uint8_t *data, size_t szData)
{
    size_t i;
    for (i = 0; i < szData; i++) {
        printf("%02x", data[i]);
    }
    printf("\n");
}

static void
print_error(const char *fmt, ...)
{
    va_list args;

    fprintf(stderr, "[Error] ");
    va_start(args, fmt);
    vfprintf(stderr, fmt, args);
    va_end(args);
    fprintf(stderr, " (%s)\n", nfc_strerror(NULL));
}

static void
print_device_error(const char *msg)
{
    fprintf(stderr, "%s: %s\n", msg, nfc_strerror(pnd));
}

static bool
transmit_bits(const uint8_t *pbtTx, const size_t szTxBits)
{
    // Show transmitted command
    if (!quiet_output) {
        printf("Sent bits:     ");
        print_hex(pbtTx, (szTxBits + 7) / 8);
    }

    // Transmit the bit frame command, we don't use the arbitrary parity feature
    if ((szRxBits = nfc_initiator_transceive_bits(pnd, pbtTx, szTxBits, NULL, abtRx, sizeof(abtRx), NULL)) < 0)
        return false;

    // Show received answer
    if (!quiet_output) {
        printf("Received bits: ");
        print_hex(abtRx, (szRxBits + 7) / 8);
    }

    return true;
}

static bool
transmit_bytes(const uint8_t *pbtTx, const size_t szTx)
{
    // Show transmitted command
    if (!quiet_output) {
        printf("Sent bytes:    ");
        print_hex(pbtTx, szTx);
    }

    // Transmit the command bytes
    if ((szRxBytes = nfc_initiator_transceive_bytes(pnd, pbtTx, szTx, abtRx, sizeof(abtRx), TRANSCEIVE_TIMEOUT_MS)) < 0)
        return false;

    // Show received answer
    if (!quiet_output) {
        printf("Received bytes:");
        print_hex(abtRx, szRxBytes);
    }

    return true;
}

static bool
parse_hex_block0(const char *hex, uint8_t *block0)
{
    char tmp[3];
    unsigned int c;
    size_t i;

    for (i = 0; i < BLOCK0_HEX_LEN; i++) {
        if (!isxdigit((unsigned char) hex[i]))
            return false;
    }

    for (i = 0; i < BLOCK0_LEN; i++) {
        memcpy(tmp, hex + i * 2, 2);
        tmp[2] = 0x00;
        if (sscanf(tmp, "%02x", &c) != 1)
            return false;
        block0[i] = (uint8_t) c;
    }

    return true;
}

static uint8_t
calc_bcc(const uint8_t *block0)
{
    return block0[0] ^ block0[1] ^ block0[2] ^ block0[3];
}

// Soft-bricked cards cannot complete the anticollision, so this is best effort only
static bool
get_uid(void)
{
    size_t szUid;
    uint8_t abtSak;

    abtSelectAll[0] = 0x93;
    abtSelectTag[0] = 0x93;

    if (!transmit_bits(abtReqa, 7))
        return false;

    if (!transmit_bytes(abtSelectAll, 2))
        return false;

    memcpy(abtUid, abtRx, 4);
    szUid = 4;

    memcpy(abtSelectTag + 2, abtRx, 5);
    iso14443a_crc_append(abtSelectTag, 7);
    if (!transmit_bytes(abtSelectTag, 9))
        return false;
    abtSak = abtRx[0];

    if (abtSak & CASCADE_BIT) {
        // Second cascade level (7 bytes UID)
        abtSelectAll[0] = 0x95;
        if (!transmit_bytes(abtSelectAll, 2))
            return false;

        memcpy(abtUid + 4, abtRx, 4);
        szUid = 8;

        abtSelectTag[0] = 0x95;
        memcpy(abtSelectTag + 2, abtRx, 5);
        iso14443a_crc_append(abtSelectTag, 7);
        if (!transmit_bytes(abtSelectTag, 9))
            return false;
        abtSak = abtRx[0];
    }

    printf("\nFound tag with\n UID: ");
    print_hex(abtUid, szUid);
    printf(" SAK: %02x\n", abtSak);

    return true;
}

// The magic wakeup is sent first, so a corrupted block0 does not block the recovery
static bool
magic_wakeup(void)
{
    iso14443a_crc_append(abtHalt, 2);
    transmit_bytes(abtHalt, 4);

    if (!transmit_bits(abtUnlock1, 7))
        return false;

    return transmit_bytes(abtUnlock2, 1);
}

static bool
backdoor_write_block0(void)
{
    uint8_t abtBlock0Tx[BLOCK0_LEN + 2];

    if (!magic_wakeup())
        return false;

    iso14443a_crc_append(abtWriteBlock0Cmd, 2);
    if (!transmit_bytes(abtWriteBlock0Cmd, 4))
        return false;

    memcpy(abtBlock0Tx, abtNewBlock0, BLOCK0_LEN);
    iso14443a_crc_append(abtBlock0Tx, BLOCK0_LEN);
    return transmit_bytes(abtBlock0Tx, BLOCK0_LEN + 2);
}

static bool
read_block0_magic(void)
{
    if (!magic_wakeup())
        return false;

    iso14443a_crc_append(abtReadBlock0Cmd, 2);
    if (!transmit_bytes(abtReadBlock0Cmd, 4))
        return false;

    if (szRxBytes < BLOCK0_LEN)
        return false;

    memcpy(abtReadBlock0, abtRx, BLOCK0_LEN);
    return true;
}

static bool
read_block0_normal(void)
{
    if (!get_uid())
        return false;

    iso14443a_crc_append(abtReadBlock0Cmd, 2);
    if (!transmit_bytes(abtReadBlock0Cmd, 4))
        return false;

    if (szRxBytes < BLOCK0_LEN)
        return false;

    memcpy(abtReadBlock0, abtRx, BLOCK0_LEN);
    return true;
}

static bool
set_block0(void)
{
    if (!backdoor_write_block0()) {
        printf("Error: block0 write failed or not acknowledged\n");
        return false;
    }

    if (skip_verify)
        return true;

    if (!read_block0_magic() && !read_block0_normal()) {
        printf("Error: block0 read-back failed, re-read the card to confirm the result\n");
        return false;
    }

    if (memcmp(abtReadBlock0, abtNewBlock0, BLOCK0_LEN) != 0) {
        printf("Error: block0 read-back mismatch\n Expected: ");
        print_hex(abtNewBlock0, BLOCK0_LEN);
        printf(" Got     : ");
        print_hex(abtReadBlock0, BLOCK0_LEN);
        return false;
    }

    printf("block0 read-back verified\n");
    return true;
}

static void
print_usage(char *argv[])
{
    printf("Usage: %s [OPTIONS] <BLOCK0_HEX>\n", argv[0]);
    printf("Options:\n");
    printf("\t-h\tHelp. Print this message.\n");
    printf("\t-q\tQuiet mode. Suppress output of READER and CARD data (improves timing).\n");
    printf("\t-f\tForce write even if block0 BCC is incorrect.\n");
    printf("\t-n\tSkip the block0 read-back verification.\n");
    printf("\n\tBLOCK0_HEX is the 32 HEX chars of the target block0.\n");
    printf("\n\tThis utility will set block0 (UID) of special Mifare 1K cards (Chinese clones) aka UFUID.\n");
    printf("\tIt only works before the card is sealed (locked) by nfc-mflock.\n\n");
}

int
main(int argc, char *argv[])
{
    int arg;
    bool has_block0 = false;

    for (arg = 1; arg < argc; arg++) {
        if (0 == strcmp(argv[arg], "-h")) {
            print_usage(argv);
            exit(EXIT_SUCCESS);
        } else if (0 == strcmp(argv[arg], "-q")) {
            quiet_output = true;
        } else if (0 == strcmp(argv[arg], "-f")) {
            force_bcc = true;
        } else if (0 == strcmp(argv[arg], "-n")) {
            skip_verify = true;
        } else if (strlen(argv[arg]) == BLOCK0_HEX_LEN && !has_block0) {
            if (!parse_hex_block0(argv[arg], abtNewBlock0)) {
                print_error("%s is not a valid block0 hex string.", argv[arg]);
                print_usage(argv);
                exit(EXIT_FAILURE);
            }
            has_block0 = true;
        } else {
            print_error("%s is not a supported option.", argv[arg]);
            print_usage(argv);
            exit(EXIT_FAILURE);
        }
    }

    if (!has_block0) {
        print_usage(argv);
        exit(EXIT_FAILURE);
    }

    if (abtNewBlock0[BCC_INDEX] != calc_bcc(abtNewBlock0)) {
        printf("Error: block0 BCC is invalid (expected %02x, got %02x)\n", calc_bcc(abtNewBlock0), abtNewBlock0[BCC_INDEX]);
        if (!force_bcc) {
            printf("Refusing to write, an invalid BCC can brick the card. Use -f to force.\n");
            exit(EXIT_FAILURE);
        }
        printf("Warning: forcing the write anyway\n");
    }

    printf("Target block0: ");
    print_hex(abtNewBlock0, BLOCK0_LEN);

    nfc_context *context;
    nfc_init(&context);
    if (context == NULL) {
        print_error("Unable to init libnfc (malloc)");
        exit(EXIT_FAILURE);
    }

    // Try to open the NFC reader
    pnd = nfc_open(context, NULL);
    if (pnd == NULL) {
        print_error("Error opening NFC reader");
        nfc_exit(context);
        exit(EXIT_FAILURE);
    }

    // Initialise NFC device as "initiator"
    if (nfc_initiator_init(pnd) < 0) {
        print_device_error("nfc_initiator_init");
        nfc_close(pnd);
        nfc_exit(context);
        exit(EXIT_FAILURE);
    }

    // Configure the CRC
    if (nfc_device_set_property_bool(pnd, NP_HANDLE_CRC, false) < 0) {
        print_device_error("nfc_device_set_property_bool");
        nfc_close(pnd);
        nfc_exit(context);
        exit(EXIT_FAILURE);
    }

    // Use raw send/receive methods
    if (nfc_device_set_property_bool(pnd, NP_EASY_FRAMING, false) < 0) {
        print_device_error("nfc_device_set_property_bool");
        nfc_close(pnd);
        nfc_exit(context);
        exit(EXIT_FAILURE);
    }

    // Disable 14443-4 autoswitching
    if (nfc_device_set_property_bool(pnd, NP_AUTO_ISO14443_4, false) < 0) {
        print_device_error("nfc_device_set_property_bool");
        nfc_close(pnd);
        nfc_exit(context);
        exit(EXIT_FAILURE);
    }

    printf("NFC reader: %s opened\n", nfc_device_get_name(pnd));

    get_uid();

    if (!set_block0()) {
        nfc_close(pnd);
        nfc_exit(context);
        exit(EXIT_FAILURE);
    }

    printf("\nblock0 set successfully\n");
    nfc_close(pnd);
    nfc_exit(context);
    exit(EXIT_SUCCESS);
}
