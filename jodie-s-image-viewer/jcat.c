// clang -O2 jcat.c -o jcat -nostartfiles -ffreestanding -nostdlib
// -fno-stack-protector -static -Wl,-n -fdata-sections -ffunction-sections
// -Wl,-gc-sections -s

__attribute__((naked)) void _start() {
    asm("xorq %rbp,%rbp\n"             // zero
        "movq 0(%rsp),%rdi\n"          // argc
        "leaq 8(%rsp),%rsi\n"          // argv
        "leaq 16(%rsp,%rdi,8), %rdx\n" // envp
        "andq $-16, %rsp\n"            // align to 16 bytes
        "call main\n"
        "movq %rax,%rdi\n"
        "movq $60,%rax\n" // exit
        "syscall");
}

typedef unsigned long size_t;
typedef unsigned long dev_t;
typedef unsigned long ino_t;
typedef unsigned long nlink_t;

typedef long blksize_t;
typedef long blkcnt_t;
typedef long off_t;

typedef unsigned mode_t;
typedef unsigned uid_t;
typedef unsigned gid_t;

#define NULL 0

size_t splice(int fd_in, off_t *off_in, int fd_out, off_t *off_out, size_t len,
              unsigned int flags) {
    register off_t *r10 asm("r10") = off_out;
    register size_t r8 asm("r8") = len;
    register unsigned int r9 asm("r9") = flags;

    size_t result;
    asm volatile("syscall"
                 : "=a"(result)
                 : "0"(275), "D"(fd_in), "S"(off_in), "d"(fd_out), "r"(r10),
                   "r"(r8), "r"(r9)
                 : "rcx", "r11", "memory");
    return result;
}

struct timespec {
    long a;
    long b;
};

struct stat {
    dev_t st_dev;
    ino_t st_ino;
    nlink_t st_nlink;

    mode_t st_mode;
    uid_t st_uid;
    gid_t st_gid;
    unsigned int __pad0;

    dev_t st_rdev;
    off_t st_size;
    blksize_t st_blksize;
    blkcnt_t st_blocks;

    struct timespec st_atim;
    struct timespec st_mtim;
    struct timespec st_ctim;
    long __unused[3];
};

int fstat(int fd, struct stat *statbuf) {
    size_t result;
    asm volatile("syscall"
                 : "=a"(result)
                 : "0"(5), "D"(fd), "S"(statbuf)
                 : "rcx", "r11", "memory");
    return result;
}

int close(int fd) {
    size_t result;
    asm volatile("syscall"
                 : "=a"(result)
                 : "0"(3), "D"(fd)
                 : "rcx", "r11", "memory");
    return result;
}

int pipe(int *fd) {
    size_t result;
    asm volatile("syscall"
                 : "=a"(result)
                 : "0"(22), "D"(fd)
                 : "rcx", "r11", "memory");
    return result;
}

int open(const char *filename, int flags, int mode) {
    size_t result;
    asm volatile("syscall"
                 : "=a"(result)
                 : "0"(2), "D"(filename), "S"(flags), "d"(mode)
                 : "rcx", "r11", "memory");
    return result;
}

size_t write(int fd, void *buf, size_t size) {
    size_t result;
    asm volatile("syscall"
                 : "=a"(result)
                 : "0"(1), "D"(fd), "S"(buf), "d"(size)
                 : "rcx", "r11", "memory");
    return result;
}

// shamelessly stolen from musl
size_t strlen(const char *s) {
    const char *a = s;
    for (; *s; s++)
        ;
    return s - a;
}

#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define O_RDONLY 00
#define O_WRONLY 01
#define O_RDWR 02

#define S_IFMT 0170000
#define S_IFDIR 0040000
#define S_IFCHR 0020000
#define S_IFBLK 0060000
#define S_IFREG 0100000
#define S_IFIFO 0010000
#define S_IFLNK 0120000
#define S_IFSOCK 0140000

#define S_ISDIR(mode) (((mode)&S_IFMT) == S_IFDIR)

int is_directory(int fd) {
    struct stat buffer;
    fstat(fd, &buffer);
    return S_ISDIR(buffer.st_mode);
}

int main(int argc, char **argv) {
    int pipe_fileno[2];
    pipe(pipe_fileno);

    int filenos[argc];

    for (int i = 1; i < argc; i += 1) {
        filenos[i] = argv[i][0] == '-' && argv[i][1] == '\0'
                         ? STDIN_FILENO
                         : open(argv[i], O_RDONLY, 0);
        if (filenos[i] < 0) {
            write(STDOUT_FILENO, argv[i], strlen(argv[i]));
            write(STDOUT_FILENO, " doesn't exist\n", 15);
            return 0;
        }
        if (is_directory(filenos[i])) {
            write(STDOUT_FILENO, argv[i], strlen(argv[i]));
            write(STDOUT_FILENO, " is a directory\n", 16);
            return 0;
        }
    }

    for (int i = 1; i < argc; i += 1) {
        while (splice(filenos[i], NULL, pipe_fileno[1], NULL, 65536, 0)) {
            splice(pipe_fileno[0], NULL, STDOUT_FILENO, NULL, 65536, 0);
        }
        close(filenos[i]);
    }

    return 0;
}
