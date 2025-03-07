#pragma once

#include <stddef.h>
#include <sys/stat.h>

#define LIB_DEFAULT_MODE (S_IRWXU | S_IRWXG | S_IROTH | S_IXOTH)

#define LIB_ERROR(ERROR_NUMBER, FMT, ...) lib_error(ERROR_NUMBER, __FILE__, __LINE__, FMT, ##__VA_ARGS__)
#define LIB_WARN(ERROR_NUMBER, FMT, ...) lib_warn(ERROR_NUMBER, __FILE__, __LINE__, FMT, ##__VA_ARGS__)

#define LIB_PATH_JOIN(...) lib_path_join(__VA_ARGS__, NULL)

#define LIB_CLEANUP_FREE __attribute__((cleanup(lib_cleanup_free)))

#define LIB_OK(STATUS) (STATUS == LIB_STATUS_OK)

typedef enum {
    LIB_STATUS_FAIL,
    LIB_STATUS_OK
} lib_status_t;

void lib_error(int error_number, const char *file, size_t line, const char *fmt, ...);
void lib_warn(int error_number, const char *file, size_t line, const char *fmt, ...);

/**
    @retval `1` path does not exists
    @retval `0` path exists
    @retval `-1` error (errno set)
*/
int lib_path_exists(const char *path);

/**
 * @returns NULL on failure, path on success
 */
char *lib_path_join(const char *a, ...);

lib_status_t lib_path_make(const char *path, mode_t mode);

lib_status_t lib_path_delete(const char *path);

lib_status_t lib_path_clean(const char *path);

lib_status_t lib_path_write(const char *path, const char *data, const char *mode);

lib_status_t lib_path_copy(const char *dest, const char *src, bool warn_conflicts);

lib_status_t lib_link_recursive(const char *src, const char *dest);

void lib_cleanup_free(void *p);