#include "lib.h"

#include <assert.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <libgen.h>
#include <errno.h>
#include <unistd.h>
#include <dirent.h>
#include <limits.h>
#include <ftw.h>
#include <sys/sendfile.h>

static int rm_files(const char *pathname, const struct stat *st, int type, struct FTW *ftwb) {
    int r = remove(pathname);
    if(r < 0) LIB_ERROR(errno, "path_delete remove `%s`", pathname);
    return r;
}

static int chmod_files(const char *pathname, const struct stat *st, int type, struct FTW *ftwb) {
    if(S_ISLNK(st->st_mode)) return 0;
    int r = chmod(pathname, S_IRWXU | S_IRWXG | S_IRWXO);
    if(r < 0) LIB_ERROR(errno, "path_delete chmod `%s`", pathname);
    return r;
}

void lib_error(int error_number, const char *file, size_t line, const char *fmt, ...) {
    va_list list;
    va_start(list, message);
    fprintf(stderr, "\e[33m%s:%lu error: ", file, line);
    vfprintf(stderr, fmt, list);
    va_end(list);

    if(error_number != 0) fprintf(stderr, " (%s)", strerror(error_number));
    fprintf(stderr, "\e[0m\n");
}

void lib_warn(int error_number, const char *file, size_t line, const char *fmt, ...) {
    va_list list;
    va_start(list, message);
    fprintf(stderr, "\e[33m%s:%lu warn: ", file, line);
    vfprintf(stderr, fmt, list);
    va_end(list);

    if(error_number != 0) fprintf(stderr, " (%s)", strerror(error_number));
    fprintf(stderr, "\e[0m\n");
}

int lib_path_exists(const char *path) {
    struct stat st;
    int r = stat(path, &st);
    if(r < 0) {
        if(errno == ENOENT) return 1;
        LIB_ERROR(errno, "path_exists `%s`", path);
        return -1;
    }
    return 0;
}

int lib_path_delete(const char *path) {
    int r = nftw(path, chmod_files, 10, FTW_DEPTH | FTW_MOUNT | FTW_PHYS);
    if(r < 0) return r;
    return nftw(path, rm_files, 10, FTW_DEPTH | FTW_MOUNT | FTW_PHYS);
}

int lib_path_make(const char *path, mode_t mode) {
    int r = lib_path_exists(path);
    if(r < 0) LIB_ERROR(0, "path_make exists");
    if(r <= 0) return r;

    r = lib_path_make(dirname(strdupa(path)), mode);
    if(r < 0) return r;

    r = mkdir(path, mode);
    if(r < 0) LIB_ERROR(errno, "path_make mkdir `%s`", path);
    return r;
}

int lib_path_clean(const char *path) {
    int r = lib_path_exists(path);
    if(r < 0) {
        LIB_ERROR(0, "path_clean path_exists `%s`", path);
        return -1;
    }

    if(r == 0 && lib_path_delete(path) < 0) {
        LIB_ERROR(0, "path_clean path_delete `%s`", path);
        return -1;
    }

    if(lib_path_make(path, LIB_DEFAULT_MODE) < 0) {
        LIB_ERROR(0, "path_clean path_make `%s`", path);
        return -1;
    }

    return 0;
}

char *lib_path_join(const char *a, ...) {
    va_list list;
    va_start(list, fmt);

    char *path = strdup(a);
    size_t path_len = strlen(path);

    char *part;
    while((part = va_arg(list, char *)) != NULL) {
        size_t part_len = strlen(part);

        path = realloc(path, path_len + 1 + part_len + 1);
        path[path_len] = '/';
        memcpy(&path[path_len + 1], part, part_len);

        path_len += 1 + part_len;
    }
    va_end(list);

    path[path_len] = '\0';
    return path;
}

int lib_path_write(const char *path, const char *data, const char *mode) {
    FILE *file = fopen(path, mode);
    if(file == NULL) {
        LIB_ERROR(errno, "path_write fopen `%s`", path);
        return -1;
    }
    int r = 0;
    size_t data_len = strlen(data);
    if(fwrite(data, 1, data_len, file) != data_len) {
        LIB_ERROR(errno, "path_write fwrite `%s`", path);
        r = -1;
    }
    if(fclose(file) != 0) LIB_WARN(errno, "path_write fclose `%s`", path);
    return r;
}

int lib_path_copy(const char *dest, const char *src, bool warn_conflicts) {
    DIR *dir = opendir(src);
    if(dir == NULL) {
        LIB_ERROR(errno, "path_copy opendir `%s`", src);
        return -1;
    }

    struct dirent *de;
    while((de = readdir(dir)) != NULL) {
        if(strcmp(de->d_name, ".") == 0 || strcmp(de->d_name, "..") == 0) continue;

        LIB_CLEANUP_FREE char *src_child = LIB_PATH_JOIN(src, de->d_name);
        LIB_CLEANUP_FREE char *dest_child = LIB_PATH_JOIN(dest, de->d_name);

        struct stat st;
        if(lstat(src_child, &st) < 0) {
            LIB_ERROR(errno, "path_copy stat `%s`", src_child);
            return -1;
        }

        if(S_ISDIR(st.st_mode)) {
            if(lib_path_make(dest_child, LIB_DEFAULT_MODE) < 0) {
                LIB_ERROR(0, "path_copy path_make failure `%s`", dest_child);
                return -1;
            }

            int r = lib_path_copy(dest_child, src_child, warn_conflicts);
            if(r < 0) return r;
            continue;
        }

        if(lib_path_exists(dest_child) == 0) {
            if(warn_conflicts) LIB_WARN(0, "path_copy conflict `%s`", dest_child);
            continue;
        }

        if(S_ISLNK(st.st_mode)) {
            char buf[PATH_MAX];
            int len = readlink(src_child, buf, PATH_MAX - 1);
            if(len < 0) {
                LIB_WARN(errno, "path_copy readlink failure `%s`", dest_child);
                continue;
            }
            buf[len] = '\0';

            if(symlink(buf, dest_child) < 0) LIB_WARN(errno, "path_copy symlink failure `%s`", dest_child);
            continue;
        }

        if(!S_ISREG(st.st_mode)) {
            LIB_WARN(0, "path_copy unknown filetype `%s`", dest_child);
            continue;
        }

        FILE *src = fopen(src_child, "r"), *dest = fopen(dest_child, "w");
        if(src == NULL) {
            LIB_WARN(errno, "path_copy fopen failure `%s`", src_child);
            continue;
        }
        if(dest == NULL) {
            LIB_WARN(errno, "path_copy fopen failure `%s`", dest_child);
            continue;
        }

        if(sendfile(fileno(dest), fileno(src), 0, st.st_size) < 0) LIB_WARN(errno, "path_copy sendfile failure `%s`", dest_child);

        if(fclose(src) < 0) LIB_WARN(errno, "path_copy fclose failure `%s`", src_child);
        if(fclose(dest) < 0) LIB_WARN(errno, "path_copy fclose failure `%s`", dest_child);

        if(chmod(dest_child, st.st_mode & 07777) < 0) LIB_WARN(errno, "path_copy chmod failure `%s`", dest_child);
    }

    return 0;
}


int lib_link_recursive(const char *src, const char *dest) {
    DIR *dir = opendir(src);
    if(dir == NULL) {
        LIB_ERROR(errno, "link_recursive opendir `%s`", src);
        return -1;
    }

    struct dirent *de;
    while((de = readdir(dir)) != NULL) {
        if(strcmp(de->d_name, ".") == 0 || strcmp(de->d_name, "..") == 0) continue;

        LIB_CLEANUP_FREE char *src_child = LIB_PATH_JOIN(src, de->d_name);
        LIB_CLEANUP_FREE char *dest_child = LIB_PATH_JOIN(dest, de->d_name);

        struct stat st;
        if(lstat(src_child, &st) < 0) {
            LIB_ERROR(errno, "link_recursive stat `%s`", src_child);
            return -1;
        }

        if(S_ISDIR(st.st_mode)) {
            if(lib_path_make(dest_child, LIB_DEFAULT_MODE) < 0) {
                LIB_ERROR(0, "link_recursive path_make failure `%s`", dest_child);
                return -1;
            }

            int r = lib_link_recursive(src_child, dest_child);
            if(r < 0) return r;
            continue;
        }

        if(link(src_child, dest_child) != 0) LIB_WARN(errno, "link_recursive link failed `%s`", dest_child);
    }

    if(closedir(dir) != 0) LIB_WARN(errno, "link_recursive closedir failed `%s`", src);

    return 0;
}

void lib_cleanup_free(void *p) {
    free(*(void**) p);
}