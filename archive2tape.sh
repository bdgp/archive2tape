#!/usr/bin/env bash
./target/release/archive2tape --db /srv/tape-archives/db/teirex.db --log /srv/tape-archives/log/teirex.log --create-db backup /data/teirex
./target/release/archive2tape --db /srv/tape-archives/db/analysis.db --log /srv/tape-archives/log/analysis.log --create-db backup /data/analysis
./target/release/archive2tape --db /srv/tape-archives/db/seqdata.db --log /srv/tape-archives/log/seqdata.log --create-db backup /data/seqdata

./k8s/rootfs/bin/sqlite-backup
rsync -av /srv/tape-archives/log /backup/
