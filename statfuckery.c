#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>

int main() {
    printf("align: %zu size: %zu\n", _Alignof(struct stat),
           sizeof(struct stat));
    printf("st_dev: %zu size:%zu\n", offsetof(struct stat, st_dev),
    sizeof(((struct stat){0}).st_dev));
    printf("st_ino: %zu size:%zu\n", offsetof(struct stat, st_ino),
    sizeof(((struct stat){0}).st_ino));
    printf("st_nlink: %zu size:%zu\n", offsetof(struct stat, st_nlink),
    sizeof(((struct stat){0}).st_nlink));
    printf("st_mode: %zu size:%zu\n", offsetof(struct stat, st_mode),
    sizeof(((struct stat){0}).st_mode));
    printf("st_uid: %zu size:%zu\n", offsetof(struct stat, st_uid),
    sizeof(((struct stat){0}).st_uid));
    printf("st_gid: %zu size:%zu\n", offsetof(struct stat, st_gid),
    sizeof(((struct stat){0}).st_gid));
    printf("__pad0: %zu size:%zu\n", offsetof(struct stat, __pad0),
    sizeof(((struct stat){0}).__pad0));
    printf("st_rdev: %zu size:%zu\n", offsetof(struct stat, st_rdev),
    sizeof(((struct stat){0}).st_rdev));
    printf("st_size: %zu size:%zu\n", offsetof(struct stat, st_size),
    sizeof(((struct stat){0}).st_size));
    printf("st_blksize: %zu size:%zu\n", offsetof(struct stat, st_blksize),
    sizeof(((struct stat){0}).st_blksize));
    printf("st_blocks: %zu size:%zu\n", offsetof(struct stat, st_blocks),
    sizeof(((struct stat){0}).st_blocks));
    printf("st_atim: %zu size:%zu\n", offsetof(struct stat, st_atim),
    sizeof(((struct stat){0}).st_atim));
    printf("st_mtim: %zu size:%zu\n", offsetof(struct stat, st_mtim),
    sizeof(((struct stat){0}).st_mtim));
    printf("st_ctim: %zu size:%zu\n", offsetof(struct stat, st_ctim),
    sizeof(((struct stat){0}).st_ctim));

    return 0;
}
