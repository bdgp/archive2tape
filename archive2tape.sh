#!/usr/bin/env bash
if (( $EUID != 0 )); then
    echo "Please run as root"
    exit
fi
set -x

#(cd /data/iarpa && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/iarpa.db --log /srv/tape-archives/log/iarpa.log --create-db backup /data/iarpa
#(cd /data/smorf && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/smorf.db --log /srv/tape-archives/log/smorf.log --create-db backup /data/smorf
(cd /data/teirex && mountpoint .) && ./target/release/archive2tape --trace --db /srv/tape-archives/db/teirex.db --log /srv/tape-archives/log/teirex.log --create-db backup /data/teirex
#(cd /mnt/local/teirex && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/mnt-local-teirex.db --log /srv/tape-archives/log/mnt-local-teirex.log --create-db backup /mnt/local/teirex
#(cd /data/flash_ldrd && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/flash_ldrd.db --log /srv/tape-archives/log/flash_ldrd.log --create-db backup /data/flash_ldrd
#(cd /data/analysis && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/analysis.db --log /srv/tape-archives/log/analysis.log --create-db backup /data/analysis
#(cd /data/seqdata && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/seqdata.db --log /srv/tape-archives/log/seqdata.log --create-db backup /data/seqdata
#(cd /data/insitu-public && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/insitu-public.db --log /srv/tape-archives/log/insitu-public.log --create-db backup /data/insitu-public
#(cd /data/insitu-production && mountpoint .) && ./target/release/archive2tape --db /srv/tape-archives/db/insitu-production.db --log /srv/tape-archives/log/insitu-production.log --create-db backup /data/insitu-production

./k8s/rootfs/bin/sqlite-backup
rsync -av /srv/tape-archives/log /backup/
