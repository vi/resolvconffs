# resolvconffs

Linux network namespaces allow separate networking environment for a group of processes (sharing uid or from a separate user).
DNS settings (`/etc/resolv.conf`) are however shared between all those environments, which may be inconvenient in some setups.

Typically (i.e. in `ip netns` tool) the mount (filesystem) namespace is used along with netns as a workaround, mapping distinct `/etc/netns/...` files to main `/etc/resolv.conf`. This tool provides different approach based on a FUSE filesystem which provides similar mapping without using additional mount namespace.

It works by inspecing PIDs of each programs that access the mounted `/etc/resolv.conf` and using `/proc/<pid>/ns/net` to find out which underlying file should be used and forwarding reads and writes to that file instead. Missing files may be propagated from a user-specified template file.

# Example

(untested)

```
# cp /etc/resolv.conf /etc/resolv.conf.bak
# mkdir /tmp/resolvconfs
# /opt/resolvconffs -d /etc/resolv.conf.bak /tmp/resolvconfs /etc/resolv.conf&
```


# Installation

Download a pre-built x64_64 version from Github releases or try `cargo install` or download source code and use `cargo build --release`. Copy resulting executable where you want.

Integrating the tool with distro's networking stack is out of scope for this document.


# Usage output

```
resolvconffs --help
Usage: /opt/resolvconffs [OPTIONS]

Special FUSE filesystem that maps its sole file to other files based on network namespace of process that queries the file.

Positional arguments:
  backing_directory          Directory where to look for resolv.conf-like files for each netns.
  mountpoint_file

Optional arguments:
  -h, --help
  -p, --extension EXTENSION  Filename extension. resolvconffs maps its file to <backing_directory>/<netns_identifier><postfix> (default: conf)
  -d, --default-file DEFAULT-FILE
                             In case of target file does not exist, copy this file to target instead of returning ENOENT.
  -P, --procfs PROCFS        Directory where to look up network namespace IDs based on PIDs. (default: /proc)
  -o, --fuse-opt OTHER-FUSE-OPTS
  ```

# Library usage

The project is not libified and library usage is not intended.

There is a simple reusable component named `FileMapperFs` inside, allowing implementing similar single-file filesystems based on `fuser` crate that maps the file based on `uid`, `gid` or `pid` of accessing process.
