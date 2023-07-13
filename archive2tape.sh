#!/usr/bin/env bash
#./target/release/archive2tape --db /srv/tape-archives/db/teirex.db --log /srv/tape-archives/log/teirex.log --create-db backup /data/teirex
#./target/release/archive2tape --db /srv/tape-archives/db/analysis.db --log /srv/tape-archives/log/analysis.log --create-db backup /data/analysis
#./target/release/archive2tape --db /srv/tape-archives/db/seqdata.db --log /srv/tape-archives/log/seqdata.log --create-db backup /data/seqdata
./target/release/archive2tape --db /srv/tape-archives/db/insitu-public.db --log /srv/tape-archives/log/insitu-public.log --create-db backup /data/insitu-public
./target/release/archive2tape --db /srv/tape-archives/db/insitu-production.db --log /srv/tape-archives/log/insitu-production.log --create-db backup /data/insitu-production

./k8s/rootfs/bin/sqlite-backup
rsync -av /srv/tape-archives/log /backup/
