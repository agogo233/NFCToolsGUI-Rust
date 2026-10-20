AM_CFLAGS  = @libnfc_CFLAGS@

bin_PROGRAMS = nfc-mfdict

noinst_HEADERS = crapto1.h nfc-mfdict.h mifare.h nfc-utils.h

nfc_mfdict_SOURCES = crapto1.c crypto1.c nfc-mfdict.c mifare.c nfc-utils.c
nfc_mfdict_LDADD   = @libnfc_LIBS@

dist_man_MANS = nfc-mfdict.1
