#!/usr/bin/env python3
from hashlib import sha256
from shlex import quote
import sqlite3
import re
import gzip

# open sqlite database at /srv/tape-archives/db/iarpa.db
conn = sqlite3.connect('/srv/tape-archives/db/iarpa.db')

# walk through every file/symlink/directory in /data/iarpa
import os

root_dir = '/data/iarpa'
checksums = {}
with gzip.open('data-iarpa-rsync.txt.gz', 'wt') as rsync:
    with gzip.open('data-iarpa-reflink.txt.gz', 'wt') as reflink:
        for root, dirs, files in os.walk(root_dir):
            for file in files:
                path = os.path.join(root, file)
                if os.path.islink(path):
                    path = re.sub(fr'^{re.escape(root_dir)}/', '', path)
                    print(path, file=rsync)
                else:
                    # get file size
                    file_size = os.path.getsize(os.path.join(root_dir, path))
                    # get file mtime
                    file_mtime = int(os.path.getmtime(os.path.join(root_dir, path)))
                    # check file table in database
                    cursor = conn.execute("SELECT checksum FROM file WHERE path = ? and mtime = ? and size = ? and checksum is not null", (os.path.join(root_dir, path), file_mtime, file_size))
                    file_row = cursor.fetchone()
                    if file_row:
                        # convert from blob to hex string
                        file_checksum = file_row[0].hex().lower()
                        print(f"Found checksum in db for file {path}: {file_checksum}")
                    else:
                        print(f"Running checksum for file {path}")
                        file_checksum = sha256(os.path.join(root_dir, path).encode('utf-8')).hexdigest().lower()
                    if file_checksum not in checksums:
                        checksums[file_checksum] = path
                        # remove leading /data/iarpa/
                        path = re.sub(fr'^{re.escape(root_dir)}/', '', path)
                        print(path, file=rsync)
                    else:
                        orig_file = checksums[file_checksum]
                        # Get metadata from the current file (before removing path prefix)
                        current_file_stat = os.stat(path)
                        uid = current_file_stat.st_uid
                        gid = current_file_stat.st_gid
                        mode = oct(current_file_stat.st_mode)[-3:]  # Get last 3 digits for permissions
                        mtime = int(current_file_stat.st_mtime)
                        # remove leading /data/iarpa/
                        orig_file = re.sub(fr'^{re.escape(root_dir)}/', '', orig_file)
                        path = re.sub(fr'^{re.escape(root_dir)}/', '', path)
                        print(f"mkdir -p {quote(os.path.dirname(path))} && cp -av --reflink {quote(orig_file)} {quote(path)} && chown {uid}:{gid} {quote(path)} && chmod {mode} {quote(path)} && touch -d '@{mtime}' {quote(path)}", file=reflink)

# ./move-data-iarpa-to-bam.py |tee move-data-iarpa-to-bam.log

# rsync -av --rsync-path='sudo /usr/local/bin/rsync' data-iarpa-rsync.txt.gz admin@bam:/volume1/iarpa/

# ssh -t admin@bam 'sudo /usr/local/bin/rsync -av --rsync-path="sudo rsync" --files-from=<(gzip -cd data-iarpa-rsync.txt.gz) admin@192.168.0.49:/share/iarpa/ /volume1/iarpa/'

# gzip -cd data-iarpa-reflink.txt.gz | ssh admin@bam 'cd /volume1/iarpa && sudo bash -x'

