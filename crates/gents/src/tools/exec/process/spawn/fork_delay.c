#include <fcntl.h>
#include <spawn.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

static _Atomic unsigned int spawns = 0;
static _Thread_local int first_parent_waiting;

static void wait_for_file(const char *path) {
    for (unsigned int attempt = 0; attempt < 10000; attempt++) {
        if (access(path, F_OK) == 0) return;
        struct timespec delay = { .tv_nsec = 1000000 };
        nanosleep(&delay, NULL);
    }
    _exit(91);
}

/* Hold the first spawn's parent-side writer until the second child exists.
 * A forked second child then retains that unrelated writer before exec. */
static void record_spawn(unsigned int ordinal) {
    const char *root = getenv("GENTS_CAPTURE_SPAWN_FIXTURE");
    if (!root || ordinal > 2) return;
    char path[4096];
    snprintf(path, sizeof(path), "%s/%s", root, ordinal == 1 ? "first" : "second");
    int fd = open(path, O_CREAT | O_WRONLY, 0600);
    if (fd < 0) _exit(90);
    close(fd);
    if (ordinal == 1) {
        snprintf(path, sizeof(path), "%s/second", root);
        wait_for_file(path);
    }
}

static pid_t delayed_fork(void) {
    const char *root = getenv("GENTS_CAPTURE_SPAWN_FIXTURE");
    if (!root) return fork();
    unsigned int ordinal = atomic_fetch_add(&spawns, 1) + 1;
    char second[4096];
    char release[4096];
    snprintf(second, sizeof(second), "%s/second", root);
    snprintf(release, sizeof(release), "%s/release", root);
    pid_t pid = fork();
    if (pid > 0 && ordinal == 1) first_parent_waiting = 1;
    if (pid > 0 && ordinal == 2) record_spawn(ordinal);
    if (pid == 0 && ordinal == 1) wait_for_file(second);
    if (pid == 0 && ordinal == 2) wait_for_file(release);
    return pid;
}

/* Rust closes its exec-error writer before reading the matching pipe. Start
 * the second spawn there so it retains stdout, but not that error writer. */
static ssize_t observed_read(int fd, void *buffer, size_t count) {
    if (first_parent_waiting) {
        first_parent_waiting = 0;
        record_spawn(1);
    }
    return read(fd, buffer, count);
}

static int observed_spawn(pid_t *pid, const char *file,
                          const posix_spawn_file_actions_t *actions,
                          const posix_spawnattr_t *attributes,
                          char *const argv[], char *const environment[]) {
    int result = posix_spawnp(pid, file, actions, attributes, argv, environment);
    if (result == 0) record_spawn(atomic_fetch_add(&spawns, 1) + 1);
    return result;
}

__attribute__((used)) static const struct {
    const void *replacement;
    const void *original;
} interpose[] __attribute__((section("__DATA,__interpose"))) = {
    { (const void *)delayed_fork, (const void *)fork },
    { (const void *)observed_read, (const void *)read },
    { (const void *)observed_spawn, (const void *)posix_spawnp }
};
