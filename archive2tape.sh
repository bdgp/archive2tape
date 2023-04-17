#!/usr/bin/env bash
./target/release/archive2tape --db /srv/tape-archives/db/teirex.db --log /srv/tape-archives/log/teirex.log --create-db backup /data/teirex
./k8s/rootfs/bin/sqlite-backup
rsync -av /srv/tape-archives/log /backup/
