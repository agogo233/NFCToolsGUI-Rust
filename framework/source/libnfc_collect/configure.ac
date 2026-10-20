AC_PREREQ([2.71])
AC_INIT([crypto1_bs], [1.0], [https://github.com/aczid/crypto1_bs])
AC_CONFIG_SRCDIR([solve_piwi_bs.c])
AC_CONFIG_HEADERS([config.h])

# Checks for programs.
AC_PROG_CC

# Checks for libraries.
AC_CHECK_LIB([nfc], [nfc_open])
AC_CHECK_LIB([pthread], [pthread_self])

# Checks for header files.
AC_CHECK_HEADERS([fcntl.h inttypes.h malloc.h stdint.h unistd.h])

# Checks for typedefs, structures, and compiler characteristics.
AC_CHECK_HEADER_STDBOOL
AC_C_INLINE
AC_TYPE_INT32_T
AC_C_RESTRICT
AC_TYPE_SIZE_T
AC_TYPE_UINT32_T
AC_TYPE_UINT64_T
AC_TYPE_UINT8_T

# Checks for library functions.
AC_FUNC_MALLOC
AC_CHECK_FUNCS([alarm memset strtoul strtoull])

AC_CONFIG_FILES([Makefile])
AC_OUTPUT
