// Standalone entry point for the WebKit host subprocess.
// Calls fd_webkit_host_main(3) from host.m (the fd is set up by the parent
// process via dup2 in pre_exec).

extern void fd_webkit_host_main(int fd) __attribute__((noreturn));

int main(void) {
    fd_webkit_host_main(3);
}
