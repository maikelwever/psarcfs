PSArcFS
=======


A very crude and not very optimized FUSE handler for .psarc files.
Supports ZLIB and LZMA compression.

Caches the first 16384 bytes per file for improved GUI file explorer performance.
Attempts to decompress the entire file into memory because of oddness with FUSE(-rs?) I do not understand.


Installation
------------

Install rust and cargo, either via your distro's package manager, or using rustup.

Also ensure you have FUSE installed, and the `fusermount` tool available (most distros have by default).

Next: run `cargo install --git https://github.com/maikelwever/psarcfs`


Usage
-----

`psarcfs <file.psarc> <mountpoint>`

Press control+c to umount filesystem and terminate psarcfs.


Example:

`psarcfs /mnt/rac/trilogy/rc1/PS3arc.psarc /tmp/test`


```
$ mount | grep psarc
PS3arc.psarc on /tmp/test type fuse.psarc (ro,nosuid,nodev,relatime,user_id=1000,group_id=1000)
```

