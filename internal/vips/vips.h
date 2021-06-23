#include <stdlib.h>
#include <string.h>
// clang-format off
#include <vips/vips.h>
#include <vips/foreign.h>
// clang-format on

char **
get_suffixes(void) {
  return vips_foreign_get_suffixes();
}

static int len_chars(char **arr) {
  int i = 0;
  while (arr[i] != NULL) {
    i++;
  }
  return i + 1;  // count NULL
}

static void
free_str_array(char **arr) {
  g_strfreev(arr);
}
