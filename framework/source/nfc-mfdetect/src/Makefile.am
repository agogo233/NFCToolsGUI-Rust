AM_CFLAGS  = @libnfc_CFLAGS@

bin_PROGRAMS = nfc-mfdetect

noinst_HEADERS = crapto1.h nfc-mfdetect.h mifare.h nfc-utils.h

nfc_mfdetect_SOURCES = crapto1.c crypto1.c nfc-mfdetect.c mifare.c nfc-utils.c
nfc_mfdetect_LDADD   = @libnfc_LIBS@

dist_man_MANS = nfc-mfdetect.1
