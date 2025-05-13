#!/usr/bin/env python3
from hashlib import sha256
from shlex import quote
import sqlite3
import re

# open sqlite database at /srv/tape-archives/db/iarpa.db
conn = sqlite3.connect('/srv/tape-archives/db/iarpa.db')

# walk through every file/symlink/directory in /data/iarpa
import os

root_dir = '/data/iarpa'
checksums = {}
with open('data-iarpa-rsync.txt', 'w') as rsync:
    with open('data-iarpa-reflink.txt', 'w') as reflink:
        for root, dirs, files in os.walk(root_dir):
            for file in files:
                path = os.path.join(root, file)
                if os.path.islink(path):
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
                        # remove leading /data/iarpa/
                        orig_file = re.sub(fr'^{re.escape(root_dir)}/', '', orig_file)
                        path = re.sub(fr'^{re.escape(root_dir)}/', '', path)
                        print(f"cp -av --reflink {quote(orig_file)} {quote(path)}", file=reflink)

# ./move-data-iarpa-to-bam.py |tee move-data-iarpa-to-bam.log

# ssh -t bam 'sudo /usr/local/bin/rsync -av --rsync-path="sudo rsync" --files-from=- 192.168.0.49:/share/iarpa/ /volume1/iarpa' <data-iarpa-rsync.txt

# ssh bam 'cd /volume1/iarpa && sudo bash -x' < data-iarpa-reflink.txt