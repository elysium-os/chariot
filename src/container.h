#pragma once

typedef struct {
    const char *name, *value;
} container_environment_variable_t;

typedef struct {
    const char *src_path, *dest_path;
    bool read_only;
} container_mount_t;

typedef struct {
    struct {
        const char *path;
        bool read_only;
    } rootfs;
    const char *cwd;
    int uid, gid;
    struct {
        container_environment_variable_t *variables;
        int variable_count;
    } environment;
    container_mount_t *mounts;
    int mount_count;
    bool silence_stdout;
    bool silence_stderr;
} container_context_t;

int container_exec(
    const char *rootfs,
    bool rootfs_read_only,
    int uid,
    int gid,
    int environment_variable_count,
    container_environment_variable_t *environment_variables,
    const char *cwd,
    container_mount_t *mounts,
    int mount_count,
    bool silence_stdout,
    bool silence_stderr,
    int arg_count,
    const char **args
);

container_context_t *container_context_make(const char *rootfs, const char *cwd);
void container_context_set_rootfs_readonly(container_context_t *context, bool read_only);
void container_context_set_ids(container_context_t *context, int uid, int gid);
void container_context_set_cwd(container_context_t *context, const char *cwd);
void container_context_set_silence(container_context_t *context, bool stdout, bool stderr);
void container_context_env_clear(container_context_t *context);
void container_context_env_add(container_context_t *context, const char *name, const char *value);
void container_context_mounts_clear(container_context_t *context);
void container_context_mounts_add(container_context_t *context, const char *from, const char *to, bool read_only);
void container_context_mounts_addm(container_context_t *context, container_mount_t mount);
void container_context_free(container_context_t *context);

int container_context_exec(container_context_t *context, int arg_count, const char **args);
int container_context_exec_shell(container_context_t *context, const char *arg);
