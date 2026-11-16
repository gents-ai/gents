#include <errno.h>
#include <stdint.h>
#include <sys/sysctl.h>
#include <sys/types.h>

/* KERN_PROC_PID retains identity for waitable zombies; libproc does not. */
int gents_darwin_process_info(int pid, int *pgid, uint64_t *seconds,
                              uint32_t *microseconds, int *zombie) {
    int mib[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, pid};
    struct kinfo_proc info = {0};
    size_t length = sizeof(info);
    if (sysctl(mib, 4, &info, &length, NULL, 0) != 0) {
        return errno == ESRCH || errno == ENOENT ? 0 : -1;
    }
    if (length == 0) return 0;
    if (length != sizeof(info) || info.kp_proc.p_pid != pid) return -1;
    *pgid = info.kp_eproc.e_pgid;
    *seconds = (uint64_t)info.kp_proc.p_un.__p_starttime.tv_sec;
    *microseconds = (uint32_t)info.kp_proc.p_un.__p_starttime.tv_usec;
    *zombie = info.kp_proc.p_stat == SZOMB;
    return 1;
}
