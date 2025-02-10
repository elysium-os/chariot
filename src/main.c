#include "config.h"
#include "lib.h"
#include "container.h"

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <dirent.h>
#include <unistd.h>
#include <getopt.h>

#define PATH_CACHE ".chariot-cache"
#define PATH_SETS (PATH_CACHE "/sets")
#define PATH_SETS_ROOTFS (PATH_CACHE "/sets/rootfs")

typedef struct {
    const char *name, *value;
} embed_variable_t;

static char *embed_variables(const char *original, size_t variable_count, embed_variable_t *variables, size_t user_variable_count, embed_variable_t *user_variables) {
    char *str = strdup(original);
    size_t str_length = strlen(str);

    bool embed = false;
    size_t embed_start = 0;
    for(size_t i = 0; i < str_length; i++) {
        if(embed) {
            if(str[i] == ')') {
                size_t embed_length = i - embed_start + 1;

                bool optional = false;
                if(str[i - 1] == '?') optional = true;

                assert(embed_length >= 3);
                if(embed_length == 3) continue;

                size_t embed_offset = 3;
                if(optional) embed_offset++;

                const char *insert = NULL;
                for(size_t j = 0; j < variable_count; j++) {
                    if(embed_length - embed_offset != strlen(variables[j].name)) continue;
                    if(strncasecmp(&str[embed_start + 2], variables[j].name, embed_length - embed_offset) != 0) continue;
                    insert = variables[j].value;
                    break;
                }
                for(size_t j = 0; j < user_variable_count; j++) {
                    if(embed_length - embed_offset != strlen(user_variables[j].name)) continue;
                    if(strncasecmp(&str[embed_start + 2], user_variables[j].name, embed_length - embed_offset) != 0) continue;
                    insert = user_variables[j].value;
                    break;
                }
                if(insert == NULL) {
                    if(optional) {
                        size_t new_str_length = str_length - embed_length;
                        memmove(&str[embed_start], &str[embed_start + embed_length], str_length - (embed_start + embed_length) + 1);
                        str = realloc(str, new_str_length + 1);
                        str[new_str_length] = '\0';
                        str_length = new_str_length;
                        embed = false;
                        continue;
                    }
                    LIB_ERROR(0, "unknown embed `%.*s`", embed_length - 3, &str[embed_start + 2]);
                    free(str);
                    return NULL;
                }
                size_t insert_length = strlen(insert);

                size_t new_str_length = str_length - embed_length + insert_length;
                if(new_str_length > str_length) str = realloc(str, new_str_length + 1);
                memmove(&str[embed_start + insert_length], &str[embed_start + embed_length], str_length - (embed_start + embed_length) + 1);
                if(new_str_length < str_length) str = realloc(str, new_str_length + 1);
                memcpy(&str[embed_start], insert, insert_length);
                str_length = new_str_length;
                embed = false;
            }
            continue;
        }
        if(str[i] != '@') continue;
        embed_start = i;
        if(i < str_length && str[++i] == '(') embed = true;
        continue;
    }

    return str;
}

static int install_rootfs(const char *rootfs_path) {
    if(lib_path_make(rootfs_path, LIB_DEFAULT_MODE) < 0) return -1;

    char *download_cmd = strdup("wget -qO- https://archive.archlinux.org/iso/2024.08.01/archlinux-bootstrap-x86_64.tar.zst | tar --strip-components 1 -x --zstd -C ");
    size_t cmd_len = strlen(download_cmd);
    size_t rootfs_len = strlen(rootfs_path);
    download_cmd = realloc(download_cmd, cmd_len + rootfs_len + 1);
    memcpy(&download_cmd[cmd_len], rootfs_path, rootfs_len);
    download_cmd[cmd_len + rootfs_len] = '\0';
    if(system(download_cmd) != 0) return -1;

    container_context_t *cc = container_context_make(rootfs_path, "/root");
    if(container_context_exec_shell(cc, "echo 'Server = https://archive.archlinux.org/repos/2024/08/01/$repo/os/$arch' > /etc/pacman.d/mirrorlist") != 0) return -1;
    if(container_context_exec_shell(cc, "echo 'en_US.UTF-8 UTF-8' > /etc/locale.gen") != 0) return -1;
    if(container_context_exec_shell(cc, "locale-gen") != 0) return -1;
    if(container_context_exec_shell(cc, "pacman-key --init") != 0) return -1;
    if(container_context_exec_shell(cc, "pacman-key --populate archlinux") != 0) return -1;
    if(container_context_exec_shell(cc, "pacman --noconfirm -Sy archlinux-keyring") != 0) return -1;
    if(container_context_exec_shell(cc, "pacman --noconfirm -S pacman pacman-mirrorlist") != 0) return -1;
    if(container_context_exec_shell(cc, "pacman --noconfirm -Syu") != 0) return -1;
    if(container_context_exec_shell(cc, "pacman --noconfirm -S bison diffutils docbook-xsl flex gettext inetutils libtool libxslt m4 make patch perl python texinfo w3m which wget xmlto") != 0) return -1;

    return 0;
}

static int qsort_strcmp(const void *a, const void *b) {
    return strcmp(*(const char **) a, *(const char **) b);
}

static int link_recursive(const char *src, const char *dest) {
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

            int r = link_recursive(src_child, dest_child);
            if(r < 0) return r;
            continue;
        }

        if(link(src_child, dest_child) != 0) LIB_WARN(errno, "link_recursive link failed `%s`", dest_child);
    }

    if(closedir(dir) != 0) LIB_WARN(errno, "link_recursive closedir failed `%s`", src);

    return 0;
}

static char *image_deps(const char *sets_path, size_t dep_count, const char **deps) {
    char *path = strdup(sets_path);
    for(size_t i = 0; i < dep_count; i++) {
        char *set_path = LIB_PATH_JOIN(path, deps[i]);

        if(lib_path_exists(set_path) != 0) {
            LIB_CLEANUP_FREE char *parent_root = LIB_PATH_JOIN(path, "rootfs");
            LIB_CLEANUP_FREE char *set_root = LIB_PATH_JOIN(set_path, "rootfs");

            if(link_recursive(parent_root, set_root) != 0) {
                LIB_ERROR(0, "image_deps failed");
                lib_path_delete(set_path);
                return NULL;
            }

            container_context_t *cc = container_context_make(set_root, "/root");

            if(container_context_exec(cc, 4, (const char *[]) { "/usr/bin/pacman", "--noconfirm", "-S", deps[i] }) != 0) {
                LIB_ERROR(0, "image_deps failed to install `%s`", deps[i]);
                lib_path_delete(set_path);
                return NULL;
            }

            container_context_free(cc);
        }

        free(path);
        path = set_path;
        skip:
    }
    return path;
}

static int install_deps(recipe_t *recipe, bool runtime, const char *source_deps_dir, const char *host_deps_dir, const char *target_deps_dir, const char ***image_dependencies, size_t *image_dependency_count, recipe_list_t *installed, bool warn_conflicts) {
    const char **image_deps = *image_dependencies;
    size_t image_dep_count = *image_dependency_count;

    for(size_t i = 0; i < recipe->dependency_count; i++) {
        if(runtime && !recipe->dependencies[i].runtime) continue;

        recipe_t *dependency = recipe->dependencies[i].resolved;
        if(recipe_list_find(installed, dependency)) continue;

        LIB_CLEANUP_FREE char *dependency_dir = LIB_PATH_JOIN(PATH_CACHE, recipe_namespace_stringify(dependency->namespace), dependency->name);
        LIB_CLEANUP_FREE char *source_src_dir = LIB_PATH_JOIN(dependency_dir, "src");
        LIB_CLEANUP_FREE char *host_install_dir = LIB_PATH_JOIN(dependency_dir, "install", "usr", "local");
        LIB_CLEANUP_FREE char *target_install_dir = LIB_PATH_JOIN(dependency_dir, "install");

        LIB_CLEANUP_FREE char *source_dep_dir = LIB_PATH_JOIN(source_deps_dir, dependency->name);

        switch(dependency->namespace) {
            case RECIPE_NAMESPACE_SOURCE: if(lib_path_make(source_dep_dir, LIB_DEFAULT_MODE) < 0 || lib_path_copy(source_dep_dir, source_src_dir, warn_conflicts) < 0) goto error; break;
            case RECIPE_NAMESPACE_HOST: if(lib_path_copy(host_deps_dir, host_install_dir, warn_conflicts) < 0) goto error; break;
            case RECIPE_NAMESPACE_TARGET: if(lib_path_copy(target_deps_dir, target_install_dir, warn_conflicts) < 0) goto error; break;
            error:
                LIB_ERROR(0, "failed to install dependency `%s/%s` for recipe `%s/%s`", recipe_namespace_stringify(dependency->namespace), dependency->name, recipe_namespace_stringify(recipe->namespace), recipe->name);
                return -1;
        }

        recipe_list_add(installed, dependency);
        if(install_deps(dependency, true, source_deps_dir, host_deps_dir, target_deps_dir, &image_deps, &image_dep_count, installed, warn_conflicts) < 0) return -1;
    }

    for(size_t i = 0; i < recipe->image_dependency_count; i++) {
        image_dependency_t *dep = &recipe->image_dependencies[i];
        if(runtime && !dep->runtime) continue;

        for(size_t j = 0; j < image_dep_count; j++) {
            if(strcmp(dep->name, image_deps[j]) == 0) goto skip;
        }
        image_deps = reallocarray(image_deps, ++image_dep_count, sizeof(const char *));
        image_deps[image_dep_count - 1] = dep->name;
        skip:
    }

    *image_dependencies = image_deps;
    *image_dependency_count = image_dep_count;
    return 0;
}

static int process_recipe(recipe_t *recipe, size_t user_variable_count, embed_variable_t *user_variables, bool verbose, bool warn_conflicts) {
    if((recipe->namespace == RECIPE_NAMESPACE_HOST || recipe->namespace == RECIPE_NAMESPACE_TARGET) && recipe->host_target.source.resolved != NULL) {
        if(process_recipe(recipe->host_target.source.resolved, user_variable_count, user_variables, verbose, warn_conflicts) < 0) return -1;
    }
    for(size_t i = 0; i < recipe->dependency_count; i++) {
        assert(recipe->dependencies[i].resolved != NULL);
        if(process_recipe(recipe->dependencies[i].resolved, user_variable_count, user_variables, verbose, warn_conflicts) < 0) return -1;
    }

    LIB_CLEANUP_FREE char *recipe_dir = LIB_PATH_JOIN(PATH_CACHE, recipe_namespace_stringify(recipe->namespace), recipe->name);
    bool recipe_dir_exists = lib_path_exists(recipe_dir) == 0;

    if(recipe->status.built || (recipe_dir_exists && !recipe->status.invalidated)) return 0;
    printf("> %s/%s\n", recipe_namespace_stringify(recipe->namespace), recipe->name);

    // Generate dependency directories
    LIB_CLEANUP_FREE char *source_deps_dir = LIB_PATH_JOIN(PATH_CACHE, "deps", "source");
    LIB_CLEANUP_FREE char *host_deps_dir = LIB_PATH_JOIN(PATH_CACHE, "deps", "host");
    LIB_CLEANUP_FREE char *target_deps_dir = LIB_PATH_JOIN(PATH_CACHE, "deps", "target");
    if(lib_path_clean(source_deps_dir) < 0 || lib_path_clean(host_deps_dir) < 0 || lib_path_clean(target_deps_dir) < 0) {
        LIB_ERROR(0, "failed to clean deps directories");
        goto terminate;
    }

    const char **image_dependencies = NULL;
    size_t image_dependency_count = 0;

    recipe_list_t installed = RECIPE_LIST_INIT;
    if(install_deps(recipe, false, source_deps_dir, host_deps_dir, target_deps_dir, &image_dependencies, &image_dependency_count, &installed, warn_conflicts) < 0) {
        LIB_ERROR(0, "failed to install dependencies");
        goto terminate;
    }
    recipe_list_free(&installed);

    qsort(image_dependencies, image_dependency_count, sizeof(const char *), qsort_strcmp);

    // Process recipe
    char *sets_path = image_deps(PATH_SETS, image_dependency_count, image_dependencies);
    if(sets_path == NULL) return -1;
    LIB_CLEANUP_FREE char *rootfs_path = LIB_PATH_JOIN(sets_path, "rootfs");
    free(sets_path);
    free(image_dependencies);

    container_context_t *cc = container_context_make(rootfs_path, "/root");
    container_context_set_verbosity(cc, verbose);

    if(lib_path_clean(recipe_dir) < 0) {
        LIB_ERROR(0, "failed to clean recipe directory for recipe `%s/%s`", recipe_namespace_stringify(recipe->namespace), recipe->name);
        goto terminate;
    }

    container_mount_t source_deps_mount = { .dest_path = "/chariot/sources", .src_path = source_deps_dir };
    container_mount_t host_deps_mount = { .dest_path = "/usr/local", .src_path = host_deps_dir };
    container_mount_t target_deps_mount = { .dest_path = "/chariot/sysroot", .src_path = target_deps_dir };

    switch(recipe->namespace) {
        case RECIPE_NAMESPACE_SOURCE: {
            LIB_CLEANUP_FREE char *sums_path = LIB_PATH_JOIN(recipe_dir, "b2sums.txt");
            LIB_CLEANUP_FREE char *archive_path = LIB_PATH_JOIN(recipe_dir, "archive");
            LIB_CLEANUP_FREE char *src_path = LIB_PATH_JOIN(recipe_dir, "src");

            container_context_mounts_add(cc, recipe_dir, "/chariot/source", false);

            if(lib_path_make(src_path, LIB_DEFAULT_MODE) < 0) {
                LIB_ERROR(0, "failed to create src directory for source `%s`", recipe->name);
                goto terminate;
            }

            switch(recipe->source.type) {
                const char *tar_format;
                case RECIPE_SOURCE_TYPE_TAR_GZ: tar_format = "--gzip"; goto tar;
                case RECIPE_SOURCE_TYPE_TAR_XZ: tar_format = "--zstd"; goto tar;
                case RECIPE_SOURCE_TYPE_LOCAL:
                    if(lib_path_exists(recipe->source.url) != 0) {
                        LIB_ERROR(0, "local directory not found `%s` for recipe `%s`", recipe->source.url, recipe->name);
                        goto terminate;
                    }

                    if(lib_path_copy(src_path, recipe->source.url, true) < 0) {
                        LIB_ERROR(0, "local copy failed for source `%s`", recipe->name);
                        goto terminate;
                    }
                    break;
                tar:
                    if(lib_path_write(sums_path, recipe->source.b2sum, "w") < 0 || lib_path_write(sums_path, " /chariot/source/archive", "a") < 0) {
                        LIB_ERROR(0, "failed to write sums for source `%s`", recipe->name);
                        goto terminate;
                    }

                    if(container_context_exec(cc, 4, (const char *[]) { "wget", "-qO", "/chariot/source/archive", recipe->source.url }) != 0) {
                        LIB_ERROR(0, "source download failed for source `%s`", recipe->name);
                        goto terminate;
                    }

                    if(container_context_exec(cc, 3, (const char *[]) { "b2sum", "--check", "/chariot/source/b2sums.txt" }) != 0) {
                        LIB_ERROR(0, "b2sum failed for source `%s`", recipe->name);
                        goto terminate;
                    }

                    if(container_context_exec(cc, 11, (const char *[]) { "tar", "--no-same-owner", "--no-same-permissions", "--strip-components", "1", "-x", tar_format, "-C", "/chariot/source/src", "-f", "/chariot/source/archive" }) != 0) {
                        LIB_ERROR(0, "extraction failed for source `%s`", recipe->name);
                        goto terminate;
                    }
                    break;
            }

            container_mount_t src_mount = { .dest_path = "/chariot/source", .src_path = src_path };

            container_context_set_cwd(cc, "/chariot/source");
            container_context_mounts_clear(cc);
            container_context_mounts_addm(cc, src_mount);

            if(recipe->source.patch != NULL) {
                LIB_CLEANUP_FREE char *patches_path = LIB_PATH_JOIN(PATH_CACHE, "patches");
                LIB_CLEANUP_FREE char *patch_path = LIB_PATH_JOIN(patches_path, recipe->source.patch);
                if(lib_path_exists(patch_path) != 0) {
                    LIB_ERROR(0, "could not locate patch `%s`", recipe->source.patch);
                    goto terminate;
                }

                container_context_mounts_add(cc, patches_path, "/chariot/patches", false);

                LIB_CLEANUP_FREE char *local_patch_path = LIB_PATH_JOIN("/chariot/patches", recipe->source.patch);
                if(container_context_exec(cc, 4, (const char *[]) { "patch", "-p1", "-i", local_patch_path }) != 0) {
                    LIB_ERROR(0, "patch failed for source `%s`", recipe->name);
                    goto terminate;
                }
            }

            container_context_mounts_clear(cc);
            container_context_mounts_addm(cc, source_deps_mount);
            container_context_mounts_addm(cc, host_deps_mount);
            container_context_mounts_addm(cc, target_deps_mount);
            container_context_mounts_addm(cc, src_mount);

            const char *strap = recipe->source.strap;
            if(strap != NULL) {
                strap = embed_variables(strap, 1, (embed_variable_t []) {{ .name = "sources_dir", .value = "/chariot/sources" }}, user_variable_count, user_variables);
                if(strap == NULL) goto terminate;
                if(container_context_exec_shell(cc, strap) != 0) {
                    LIB_ERROR(0, "shell command failed for `%s/%s`", recipe_namespace_stringify(recipe->namespace), recipe->name);
                    goto terminate;
                }
                free((char *) strap);
            }
        } break;
        const char *prefix;
        case RECIPE_NAMESPACE_HOST: prefix = "/usr/local"; goto host_target;
        case RECIPE_NAMESPACE_TARGET: prefix = "/usr"; goto host_target;
        host_target: {
            LIB_CLEANUP_FREE char *build_path = LIB_PATH_JOIN(recipe_dir, "build");
            LIB_CLEANUP_FREE char *install_path = LIB_PATH_JOIN(recipe_dir, "install");
            char *source_path = NULL;
            if(recipe->host_target.source.resolved != NULL) source_path = LIB_PATH_JOIN(PATH_CACHE, recipe_namespace_stringify(RECIPE_NAMESPACE_SOURCE), recipe->host_target.source.name, "src");

            if(lib_path_make(build_path, LIB_DEFAULT_MODE) < 0) {
                LIB_ERROR(0, "failed to create build directory for `%s/%s`", recipe_namespace_stringify(recipe->namespace), recipe->name);
                goto terminate;
            }

            if(lib_path_make(install_path, LIB_DEFAULT_MODE) < 0) {
                LIB_ERROR(0, "failed to create install directory for `%s/%s`", recipe_namespace_stringify(recipe->namespace), recipe->name);
                goto terminate;
            }

            container_context_set_cwd(cc, "/chariot/build");
            container_context_mounts_addm(cc, source_deps_mount);
            container_context_mounts_addm(cc, host_deps_mount);
            container_context_mounts_addm(cc, target_deps_mount);
            if(source_path != NULL) container_context_mounts_add(cc, source_path, "/chariot/source", false);
            container_context_mounts_add(cc, build_path, "/chariot/build", false);
            container_context_mounts_add(cc, install_path, "/chariot/install", false);

            struct {
                embed_variable_t *embed_variables;
                size_t embed_variable_count;
                const char *command;
            } stages[] = {
                { .command = recipe->host_target.configure, .embed_variable_count = source_path != NULL ? 4 : 3, .embed_variables = (embed_variable_t[]) {
                    { .name = "prefix", .value = prefix },
                    { .name = "sysroot_dir", .value = "/chariot/sysroot" },
                    { .name = "sources_dir", .value = "/chariot/sources" },
                    { .name = "source_dir", .value = "/chariot/source" } // keep at bottom so we can drop it with variable count
                } },
                { .command = recipe->host_target.build, .embed_variable_count = source_path != NULL ? 5 : 4, .embed_variables = (embed_variable_t[]) {
                    { .name = "prefix", .value = prefix },
                    { .name = "sysroot_dir", .value = "/chariot/sysroot" },
                    { .name = "sources_dir", .value = "/chariot/sources" },
                    { .name = "thread_count", .value = "8" },
                    { .name = "source_dir", .value = "/chariot/source" } // keep at bottom so we can drop it with variable count
                } },
                { .command = recipe->host_target.install, .embed_variable_count = source_path != NULL ? 5 : 4, .embed_variables = (embed_variable_t[]) {
                    { .name = "prefix", .value = prefix },
                    { .name = "sysroot_dir", .value = "/chariot/sysroot" },
                    { .name = "sources_dir", .value = "/chariot/sources" },
                    { .name = "install_dir", .value = "/chariot/install" },
                    { .name = "source_dir", .value = "/chariot/source" } // keep at bottom so we can drop it with variable count
                } }
            };

            for(size_t i = 0; i < sizeof(stages) / sizeof(stages[0]); i++) {
                const char *cmd = stages[i].command;
                if(cmd == NULL) continue;
                if((cmd = embed_variables(cmd, stages[i].embed_variable_count, stages[i].embed_variables, user_variable_count, user_variables)) == NULL) goto terminate;
                if(container_context_exec_shell(cc, cmd) != 0) {
                    LIB_ERROR(0, "shell command failed for `%s/%s`", recipe_namespace_stringify(recipe->namespace), recipe->name);
                    goto terminate;
                }
                free((char *) cmd);
            }

            free(source_path);
        } break;
    }

    container_context_free(cc);
    recipe->status.built = true;
    return 0;

terminate:
    container_context_free(cc);
    if(lib_path_delete(recipe_dir) < 0) LIB_WARN(0, "failed to cleanup broken build, please do so manually `%s/%s`", recipe_namespace_stringify(recipe->namespace), recipe->name);
    return -1;
}

int main(int argc, char **argv) {
    char *config_path = "./config.chariot";
    char *cmd = NULL;
    bool wipe_container = false;
    bool verbose = false, conflicts = true;

    static struct option lopts[] = {
        { .name = "config", .has_arg = required_argument, .val = 1000 },
        { .name = "verbose", .has_arg = no_argument, .val = 1001 },
        { .name = "exec", .has_arg = required_argument, .val = 1002 },
        { .name = "hide-conflicts", .has_arg = no_argument, .val = 1003 },
        { .name = "var", .has_arg = required_argument, .val = 1004 },
        { .name = "wipe-container", .has_arg = no_argument, .val = 1005 },
        {}
    };

    size_t variable_count = 0;
    embed_variable_t *variables = NULL;

    int opt;
    while((opt = getopt_long(argc, argv, "", lopts, NULL)) != -1) {
        switch(opt) {
            case 1000: config_path = optarg; break;
            case 1001: verbose = true; break;
            case 1002: cmd = optarg; break;
            case 1003: conflicts = false; break;
            case 1005: wipe_container = true; break;
            case 1004:
                int key_length = 0;
                for(int i = 0; optarg[i] != '\0' && optarg[i] != '='; i++) key_length++;

                if(optarg[key_length] != '=' || optarg[key_length + 1] == '\0') {
                    LIB_WARN(0, "variable `%.*s` is missing a value", key_length, optarg);
                    break;
                }

                if(
                    strncasecmp(optarg, "thread_count", key_length) == 0 ||
                    strncasecmp(optarg, "prefix", key_length) == 0 ||
                    strncasecmp(optarg, "sysroot_dir", key_length) == 0 ||
                    strncasecmp(optarg, "sources_dir", key_length) == 0 ||
                    strncasecmp(optarg, "install_dir", key_length) == 0 ||
                    strncasecmp(optarg, "source_dir", key_length) == 0
                ) {
                    LIB_WARN(0, "variable `%.*s` is reserved", key_length, optarg);
                    break;
                }

                int value_length = 0;
                for(int i = key_length + 1; optarg[i] != '\0'; i++) value_length++;

                variables = reallocarray(variables, ++variable_count, sizeof(embed_variable_t));
                variables[variable_count - 1] = (embed_variable_t) { .name = strndup(optarg, key_length), .value = strndup(&optarg[key_length + 1], value_length) };
                break;
        }
    }

    if(cmd != NULL) {
        container_context_t *cc = container_context_make(PATH_SETS_ROOTFS, "/root");
        container_context_set_verbosity(cc, true);
        container_context_exec_shell(cc, cmd);
        container_context_free(cc);
        return EXIT_SUCCESS;
    }

    if(lib_path_exists(config_path) != 0) {
        LIB_ERROR(0, "config not found");
        return EXIT_FAILURE;
    }
    config_t *config = config_read(config_path);

    if(wipe_container && lib_path_exists(PATH_SETS_ROOTFS) > 0) if(lib_path_delete(PATH_SETS) != 0) LIB_ERROR(0, "failed to wipe container");
    if(lib_path_exists(PATH_SETS_ROOTFS) != 0 && install_rootfs(PATH_SETS_ROOTFS) < 0) {
        LIB_ERROR(0, "failed to install rootfs");
        return EXIT_FAILURE;
    }

    recipe_list_t forced_recipes = RECIPE_LIST_INIT;
    for(int i = optind; i < argc; i++) {
        recipe_namespace_t namespace;
        char *identifier;
        if(strncmp(argv[i], "source/", 7) == 0) {
            namespace = RECIPE_NAMESPACE_SOURCE;
            identifier = &argv[i][7];
        } else if(strncmp(argv[i], "host/", 5) == 0) {
            namespace = RECIPE_NAMESPACE_HOST;
            identifier = &argv[i][5];
        } else if(strncmp(argv[i], "target/", 7) == 0) {
            namespace = RECIPE_NAMESPACE_TARGET;
            identifier = &argv[i][7];
        } else {
            LIB_WARN(0, "invalid recipe `%s`", argv[i]);
            continue;
        }

        bool found = false;
        for(size_t i = 0; i < config->recipe_count; i++) {
            if(config->recipes[i]->namespace != namespace) continue;
            if(strcmp(config->recipes[i]->name, identifier) != 0) continue;
            config->recipes[i]->status.invalidated = true;
            recipe_list_add(&forced_recipes, config->recipes[i]);
            found = true;
        }
        if(!found) LIB_WARN(0, "unknown recipe `%s/%s`", recipe_namespace_stringify(namespace), identifier);
    }

    for(size_t i = 0; i < forced_recipes.recipe_count; i++) if(process_recipe(forced_recipes.recipes[i], variable_count, variables, verbose, conflicts) < 0) break;

    return EXIT_SUCCESS;
}