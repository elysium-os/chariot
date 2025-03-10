#pragma once

#include <stddef.h>

#define RECIPE_LIST_INIT ((recipe_list_t) { .recipe_count = 0, .recipes = NULL })

typedef struct recipe recipe_t;

typedef enum {
    RECIPE_NAMESPACE_SOURCE,
    RECIPE_NAMESPACE_HOST,
    RECIPE_NAMESPACE_TARGET
} recipe_namespace_t;

typedef enum {
    RECIPE_SOURCE_TYPE_TAR_GZ,
    RECIPE_SOURCE_TYPE_TAR_XZ,
    RECIPE_SOURCE_TYPE_GIT,
    RECIPE_SOURCE_TYPE_LOCAL
} recipe_source_type_t;

typedef struct {
    bool runtime;
    recipe_namespace_t namespace;
    const char *name;
    recipe_t *resolved;
} recipe_dependency_t;

typedef struct {
    bool runtime;
    const char *name;
} image_dependency_t;

struct recipe {
    recipe_namespace_t namespace;
    const char *name;
    recipe_dependency_t *dependencies;
    size_t dependency_count;
    image_dependency_t *image_dependencies;
    size_t image_dependency_count;
    union {
        struct {
            const char *url, *b2sum, *commit, *patch;
            recipe_source_type_t type;
            const char *strap;
        } source;
        struct {
            recipe_dependency_t source;
            const char *configure, *build, *install;
        } host_target;
    };
    struct {
        bool built;
        bool failed;
        bool invalidated;
    } status;
};

typedef struct {
    recipe_t **recipes;
    size_t recipe_count;
} recipe_list_t;

void recipe_list_add(recipe_list_t *list, recipe_t *recipe);
bool recipe_list_find(recipe_list_t *list, recipe_t *recipe);
void recipe_list_free(recipe_list_t *list);

const char *recipe_namespace_stringify(recipe_namespace_t namespace);
