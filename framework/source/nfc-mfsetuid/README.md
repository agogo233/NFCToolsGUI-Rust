# nfc-mfsetuid

Set block0 (UID) of special Mifare 1K cards (Chinese clones) aka UFUID.

based on [nfc-mflock](https://github.com/duament/nfc-mflock)

## Usage

    nfc-mfsetuid [-q] [-f] [-n] <BLOCK0_HEX>

- `-q` quiet mode, suppress the transceive output (improves timing)
- `-f` force write even if the block0 BCC is incorrect
- `-n` skip the block0 read-back verification

`BLOCK0_HEX` is the 32 HEX chars of the target block0.

A wrong block0 can be recovered: the magic wakeup is sent before any anticollision, so
rewriting block0 keeps working. Once the card is sealed by nfc-mflock the backdoor is
gone and this utility cannot be used anymore.

## Build

    autoreconf -vis
    ./configure LDFLAGS=-L<prefix>/lib CPPFLAGS=-I<prefix>/include prefix=<prefix>
    make && make install
