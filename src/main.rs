#![cfg_attr(feature = "cargo-clippy", allow(cyclomatic_complexity, trivial_regex))]
use std::os::unix::prelude::MetadataExt;
use std::path::PathBuf;
use std::io::BufRead;
use std::collections::HashSet;

use chrono::{DateTime, NaiveDateTime, Utc};
use num_enum::{TryFromPrimitive, IntoPrimitive};
use regex::Regex;
use glob::Pattern;

use structopt::StructOpt;
use std::vec::Vec;
use duct::cmd;

use lazy_regex::{regex_captures, regex_is_match};

use which::which;

use color_eyre::eyre::{Result, eyre};
use rusqlite::{params, Connection};
use sysinfo::{ProcessExt, SystemExt, DiskExt};
use std::os::unix::ffi::OsStrExt;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use uuid::Uuid;
use rand::{distributions::Alphanumeric, Rng};
use filetime::FileTime;
use linked_hash_map::LinkedHashMap;


// /dev/sg0 lto5 drive
// /dev/sg1 lto5 loader
// /dev/sg2 lto8 drive
// /dev/sg4 lto8 loader

#[derive(StructOpt, Debug)]
#[structopt(name = "backup2tape", about = "Incremental tape archiving tool")]
struct Options {
    //#[structopt(long = "drive", help = "Path to tape drive device", name="DEVICE", default_value="/dev/tape/by-id/scsi-35000e111c89b5001-nst")]
    #[structopt(long = "drive", help = "Path to tape drive device", name="DEVICE", default_value="/dev/sg2")]
    drive: PathBuf,
    #[structopt(long = "loader", help = "Path to loader device", name="DEVICE", default_value="/dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0")]
    loader: PathBuf,
    #[structopt(long = "mtx", help = "Path to mtx command (from mtx package)", name="PATH", default_value="mtx")]
    mtx: PathBuf,
    #[structopt(long = "mt", help = "Path to mt command (from cpio package)", name="PATH", default_value="mt")]
    mt: PathBuf,
    #[structopt(long = "ltfs", help = "Path to ltfs command", name="PATH", default_value="ltfs")]
    ltfs: PathBuf,
    #[structopt(long = "mkltfs", help = "Path to mkltfs command", name="PATH", default_value="mkltfs")]
    mkltfs: PathBuf,
    #[structopt(long = "umount", help = "Path to umount command", name="PATH", default_value="umount")]
    umount: PathBuf,
    #[structopt(long = "db", help = "Path to SQLite backup metadata database", name="PATH")]
    db: PathBuf,
    #[structopt(long = "tapes-db", help = "Path to SQLite tapes metadata database", name="PATH")]
    tapes_db: Option<PathBuf>,
    #[structopt(long = "create-db", help = "Create the metadata database if it doesn't exist")]
    create_db: bool,
    #[structopt(long = "ltfs-path", help = "Mount path for LTFS filesystem", name="PATH", default_value="/media/lto8")]
    ltfs_path: PathBuf,
    #[structopt(long = "restore-path", help = "Restore path", name="PATH", default_value=".")]
    restore_path: PathBuf,
    #[structopt(long = "backup", help = "Perform a backup")]
    backup: bool,
    #[structopt(long = "restore", help = "Perform a restore")]
    restore: bool,
    #[structopt(long = "backup-id", help = "Backup ID to use for restore (default is latest backup)", name="ID")]
    backup_id: Option<i64>,
    #[structopt(long = "backup-uuid", help = "Backup UUID to use for restore (default is latest backup)", name="UUID")]
    backup_uuid: Option<String>,
    #[structopt(long = "backup-date", help = "Backup date to use for restore (default is latest backup)", name="DATETIME")]
    backup_date: Option<String>,
    #[structopt(long = "tape", help = "List of tapes to use for backup and restore. If none specified, use any tapes", name="LABEL")]
    tapes: Vec<String>,
    #[structopt(long = "include", help = "Regex patterns to include files", name="REGEX")]
    include: Vec<regex::bytes::Regex>,
    #[structopt(long = "exclude", help = "Regex patterns to exclude files", name="REGEX")]
    exclude: Vec<regex::bytes::Regex>,
    #[structopt(long = "ignore-tape", help = "Regex patterns to ignore tapes with matching label", name="REGEX", default_value="^CLN")]
    ignore_tape: Regex,
    #[structopt(help = "Glob patterns to match files in filesystem", name="PATTERN")]
    patterns: Vec<Pattern>,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
enum StorageType {
    Drive,
    Element,
    ImportExport,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
enum StorageStatus {
    Empty,
    Full,
}

#[derive(PartialEq, Eq, Copy, Clone, Debug, PartialOrd, Ord, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum FileType {
    File = 0,
    Directory = 1,
    Symlink = 2,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct StorageElement {
    index: usize,
    label: Option<String>,
    storage_type: StorageType,
    status: StorageStatus,
    storage_element_index: Option<usize>,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct Tape {
    id: i64,
    label: String,
    is_full: bool,
    date_added: String,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct File {
    id: i64,
    path: PathBuf,
    uid: u32,
    gid: u32,
    mode: u32,
    mtime: i64,
    size: u64,
    filetype: FileType,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct Backup {
    id: i64,
    date_created: i64,
    uuid: String,
    parent_backup_id: Option<i64>,
    is_complete: bool,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct NewFile {
    id: i64,
    path: PathBuf,
    mtime: i64,
    size: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    filetype: FileType,
}


#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct FullBackup {
    backup_id: i64,
    uid: u32,
    gid: u32,
    mode: u32,
    is_deleted: bool,
    file_id: i64,
    tape_id: i64,
    path: PathBuf,
    mtime: i64,
    size: u64,
    filetype: FileType,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct BackupDiff {
    new_file: Option<NewFile>,
    full_backup: Option<FullBackup>,
}

impl Options {
    // perl -ne '
    // Data Transfer Element (\d+):Empty/ && print "D:$1:E\n";
    // Data Transfer Element (\d+):Full \(Storage Element (\d+) Loaded\)(:VolumeTag =\s*(.+))?/ && print "D:$1:F:$2:$4\n";
    // Storage Element (\d+):Empty/ && print "S:$1:E\n";
    // Storage Element (\d+):Full( :VolumeTag=(.+))?/ && print "S:$1:F:$3\n";
    // Storage Element (\d+) IMPORT.EXPORT:Empty/ && print "I:$1:E\n";
    // Storage Element (\d+) IMPORT.EXPORT:Full( :VolumeTag=(.+))?/ && print "I:$1:F:$3\n";' ${TMPFILE}
    fn loader_status(&self) -> Result<Vec<StorageElement>> {
        let mut elements = Vec::<StorageElement>::new();
        // mtx -f /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 status
        let mtx_cmd = cmd!(&self.mtx, "-f", &self.loader, "status").run()?;
        let code = match mtx_cmd.status.code() {
            Some(c) => c.to_string(),
            _ => "null".to_string(),
        };
        if ! mtx_cmd.status.success() {
            Err(eyre!("mtx command {mtx_cmd:?} failed with status {code}"))?
        }
        for line in mtx_cmd.stdout.lines() {
            let line = line?;
            if let Some((_, drive_num)) = regex_captures!(r#"^\s*Data Transfer Element (\d+):Empty$"#, &line) {
                elements.push(StorageElement {
                    index: drive_num.parse::<usize>()?,
                    label: None,
                    storage_type: StorageType::Drive,
                    status: StorageStatus::Empty,
                    storage_element_index: None,
                });
            }
            else if let Some((_, drive_num, storage_element_index, _, label)) = regex_captures!(r#"^\s*Data Transfer Element (\d+):Full \(Storage Element (\d+) Loaded\)(:VolumeTag =\s*(.+))?$"#, &line) {
                elements.push(StorageElement {
                    index: drive_num.parse::<usize>()?,
                    label: Some(label.to_string()),
                    storage_type: StorageType::Drive,
                    status: StorageStatus::Full,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                });
            }
            else if let Some((_, storage_element_index,)) = regex_captures!(r#"^\s*Storage Element (\d+):Empty$"#, &line) {
                elements.push(StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: None,
                    storage_type: StorageType::Element,
                    status: StorageStatus::Empty,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                });
            }
            else if let Some((_, storage_element_index, _, label)) = regex_captures!(r#"^\s*Storage Element (\d+):Full( :VolumeTag=(.+))?$"#, &line) {
                elements.push(StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: Some(label.to_string()),
                    storage_type: StorageType::Element,
                    status: StorageStatus::Full,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                });
            }
            else if let Some((_, storage_element_index,)) = regex_captures!(r#"^\s*Storage Element (\d+) IMPORT.EXPORT:Empty$"#, &line) {
                elements.push(StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: None,
                    storage_type: StorageType::ImportExport,
                    status: StorageStatus::Empty,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                });
            }
            else if let Some((_, storage_element_index, _, label)) = regex_captures!(r#"^\s*Storage Element (\d+) IMPORT.EXPORT:Full( :VolumeTag=(.+))?$"#, &line) {
                elements.push(StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: Some(label.to_string()),
                    storage_type: StorageType::ImportExport,
                    status: StorageStatus::Full,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                });
            }
        }
        Ok(elements)
    }

    fn is_mounted(&self) -> Result<bool> {
        let mut system = sysinfo::System::new();
        system.refresh_disks_list();
        for disk in system.disks() {
            if disk.mount_point() == self.ltfs_path {
                return Ok(true);
            }
        }
        Err(eyre!("No disk matching {ltfs_path} was found!", ltfs_path=self.ltfs_path.display()))
    }

    fn is_tape_full(&self) -> Result<bool> {
        let mut system = sysinfo::System::new();
        system.refresh_disks_list();
        for disk in system.disks_mut() {
            if disk.mount_point() == self.ltfs_path {
                return Ok(disk.available_space() == 0);
            }
        }
        Err(eyre!("No disk matching {ltfs_path} was found!", ltfs_path=self.ltfs_path.display()))
    }

    fn tape_status(&self) -> Result<Vec<Tape>> {
        let mut tapes = Vec::<Tape>::new();
        if let Some(tapes_db) = &self.tapes_db {
            let conn = Connection::open(tapes_db)?;

            let mut stmt = conn.prepare("select * from tape")?;
            let tape_iter = stmt.query_map([], |row| {
                Ok(Tape{
                    id: row.get(0)?,
                    label: row.get(1)?,
                    is_full: row.get(2)?,
                    date_added: row.get(3)?,
                })
            })?;
            for tape in tape_iter {
                let tape = tape?;
                tapes.push(tape);
            }
        }
        Ok(tapes)
    }

    fn insert_tape_record(&self, label: &str) -> Result<()> {
        if let Some(tapes_db) = &self.tapes_db {
            let conn = Connection::open(tapes_db)?;
            conn.execute(r#"insert into tape (label) values (?1) on conflict(label) do nothing "#, params![label])?;
        }
        let conn = Connection::open(&self.db)?;
        conn.execute(r#"insert into tape (label) values (?1) on conflict(label) do nothing"#, params![label])?;
        Ok(())
    }

    fn set_tape_record_as_full(&self, label: &str) -> Result<()> {
        if let Some(tapes_db) = &self.tapes_db {
            let conn = Connection::open(tapes_db)?;
            conn.execute(r#"update tape set is_full=1 where label=?1"#, params![label])?;
        }
        let conn = Connection::open(&self.db)?;
        conn.execute(r#"update tape set is_full=1 where label=?1"#, params![label])?;
        Ok(())
    }

    fn wait_for_drive(&self) -> Result<()> {
        let mut drive_is_ready = false;
        for _ in 0..300 {
            let drivestatus = cmd!("mt","-f", &self.drive, "status").run()?;
            if regex_is_match!(r#"ONLINE"#B, &drivestatus.stdout) {
                drive_is_ready = true;
                break
            }
            std::thread::sleep(std::time::Duration::new(1, 0));
        }
        if ! drive_is_ready {
            let drivestatus = cmd!("mt","-f", &self.drive, "status").run()?;
            if ! regex_is_match!(r#"ONLINE"#B, &drivestatus.stdout) {
                Err(eyre!("Drive not ready!"))?
            }
        }
        Ok(())
    }

    fn wait_for_ltfs_process_to_terminate(&self) -> Result<()> {
        let mut system = sysinfo::System::new();
        for _ in 0..600 {
            system.refresh_processes();
            let mut found_ltfs = false;
            for (_pid, process) in system.processes() {
                if process.name() == "ltfs" || 
                    regex_is_match!(r#"(^|/)ltfs$"#, &process.exe().to_string_lossy()) ||
                    regex_is_match!(r#"(^|/)ltfs$"#, &process.cmd()[0])
                {
                    found_ltfs = true;
                    break;
                }
            }
            if !found_ltfs {
                return Ok(())
            }
            std::thread::sleep(std::time::Duration::new(1, 0));
        }
        Err(eyre!("ltfs process took too long to terminate"))
    }

    fn unmount_tape(&self) -> Result<()> {
        if self.is_mounted()? {
            cmd!(&self.umount, &self.ltfs_path).run()?;
            self.wait_for_ltfs_process_to_terminate()?;
        }
        Ok(())
    }

    fn load_tape(&self, slot: usize) -> Result<()> {
        let ls = self.loader_status()?;
        let drive_slot = ls.iter().
            filter(|se| se.storage_type==StorageType::Drive).
            next().ok_or(eyre!("Could not find drive slot"))?;

        if drive_slot.status == StorageStatus::Full {
            let empty_slot = ls.iter().
                filter(|se| se.storage_type==StorageType::Drive && se.status==StorageStatus::Empty).
                next().ok_or(eyre!("Could not find an empty slot"))?;
            
            self.unmount_tape()?;
            cmd!(&self.mtx, "-f", &self.loader, "unload", empty_slot.index.to_string(), drive_slot.index.to_string()).run()?;
        }
        cmd!(&self.mtx, "-f", &self.loader, "load", slot.to_string(), drive_slot.index.to_string()).run()?;
        self.wait_for_drive()?;
        Ok(())
    }

    fn mount_tape(&self) -> Result<()> {
        if ! self.is_mounted()? {
            cmd!(&self.ltfs, "-o", format!("devname={drive}", drive=self.drive.to_string_lossy()), &self.ltfs_path).run()?;
            if ! self.is_mounted()? {
                Err(eyre!("path {ltfs_path} was not mounted", ltfs_path=self.ltfs_path.display()))?;
            }
        }
        Ok(())
    }

    fn ready_storage(&self) -> Result<StorageElement> {
        // get loader status
        let ls = self.loader_status()?;
        // get tape status from tapes db
        let ts: Vec<Tape> = self.tape_status()?;
        // get list of full tapes we know about
        let full_tapes = ts.iter().
            filter(|t| t.is_full).
            map(|t| t.label.clone()).collect::<HashSet<_>>();
        let non_full_tapes = ts.iter().
            filter(|t| !t.is_full).
            map(|t| t.label.clone()).collect::<HashSet<_>>();

        let mut tapes = ls.iter().
            filter(|se| 
                se.storage_type == StorageType::Element && 
                se.status==StorageStatus::Full &&
                if self.tapes.is_empty() {true} else {if let Some(label) = &se.label {self.tapes.contains(label)} else {true}}).
            collect::<Vec<_>>();
        tapes.sort_by_key(|t| 
            (!(if let Some(label) = &t.label {non_full_tapes.contains(label)} else {false}), 
            &t.storage_type,
            &t.label));

        // find a tape to use
        for tape in &tapes {
            if tape.storage_type == StorageType::Element || tape.storage_type == StorageType::Drive {
                if let Some(label) = &tape.label {
                    // if the tape is not marked as full
                    if ! full_tapes.contains(label) && ! self.ignore_tape.is_match(label) {
                        if tape.storage_type != StorageType::Drive {
                            // load the tape into the drive
                            self.load_tape(tape.index)?;
                        }

                        // if no db record exists for tape, attempt to format the tape
                        // if tape is already formatted, formatting will fail
                        if !non_full_tapes.contains(label) {
                            let output = cmd!(&self.mkltfs, "-d", &self.drive).unchecked().run()?;
                            match output.status.code() {
                                Some(16) => (), // already formatted
                                Some(0) => (), // format succeeded
                                code => Err(eyre!("mkltfs command returned exit status {code:?}"))?,
                            }
                            // insert a db record for the tape
                            self.insert_tape_record(&label)?;
                        }

                        // if ltfs filesystem is not mounted
                        if !self.is_mounted()? {
                            // attempt to mount the tape
                            self.mount_tape()?;
                        }

                        // check for available space
                        if self.is_tape_full()? {
                            self.set_tape_record_as_full(label)?;
                            continue;
                        }
                        return Ok((**tape).clone())
                    }
                }
            }
        }
        Err(eyre!("Could not find any available tapes to use"))
    }

    fn init_db(&self) -> Result<()> {
        let conn = Connection::open(&self.db)?;

        conn.execute_batch(r#"
        create table if not exists tape (
            id integer primary key autoincrement not null,
            label text not null unique,
            is_full integer default false not null,
            date_added timestamp default current_timestamp not null
        );
        create table if not exists backup (
            id integer primary key autoincrement not null,
            date_created timestamp default current_timestamp not null,
            uuid text not null,
            parent_backup_id integer references backup(id)
        );
        create table if not exists file (
            id integer primary key autoincrement not null,
            tape_id integer references tape(id),
            path text not null,
            mtime integer not null,
            size integer not null,
            filetype integer not null,
        );
        create index if not exists file_path_mtime_size_idx on file (path,mtime,size);
        create table if not exists backup2file (
            backup_id integer not null references backup(id),
            file_id integer not null references file(id),
            uid integer not null,
            gid integer not null,
            mode integer not null,
            is_deleted integer not null
        );
        "#)?;

        if let Some(tapes_db) = &self.tapes_db {
            let conn = Connection::open(tapes_db)?;
            conn.execute_batch(r#"
            create table if not exists tape (
                id integer primary key autoincrement not null,
                label text not null unique,
                is_full integer default false not null,
                date_added timestamp default current_timestamp not null
            )
            "#)?;
        }

        Ok(())
    }

    fn backup_files(&self) -> Result<()> {
        // initialize the backup set's sqlite database
        self.init_db()?;
        let mut conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<String,Tape>::new();
        {
            let mut stmt = conn.prepare("select * from tape")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let label: String = row.get(1)?;
                tapes.insert(label.clone(), Tape{
                    id: row.get(0)?,
                    label: label,
                    is_full: row.get(2)?,
                    date_added: row.get(3)?,
                });
            }
        }

        // get the parent backup ID
        let mut parent_backup_id = Option::<i64>::None;
        {
            let mut stmt = conn.prepare(r#"select id from backup where is_complete order by date_created desc nulls last limit 1"#)?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                parent_backup_id = Some(row.get(0)?);
            }
        }

        // create the new backup record
        let mut backup = None;
        let backup_uuid = Uuid::new_v4();
        {
            let mut stmt = conn.prepare(r#"insert into backup (uuid,parent_backup_id) values (?1,?2) returning *"#)?;
            let mut rows = stmt.query(params![backup_uuid.to_string(), parent_backup_id])?;
            if let Some(row) = rows.next()? {
                parent_backup_id = Some(row.get(0)?);
                backup = Some(Backup {
                    id: row.get(0)?,
                    date_created: row.get(1)?,
                    uuid: row.get(2)?,
                    parent_backup_id: row.get(3)?,
                    is_complete: row.get(4)?,
                });
            }
        }

        // compute the backup diffs
        let mut backup_diffs = Vec::<BackupDiff>::new();
        let mut to_copy = LinkedHashMap::<PathBuf,NewFile>::new();
        {
            let tx = conn.transaction()?;
            tx.execute_batch(r#"
                    drop table if exists new_file;
                    create table new_file (
                        id integer primary key autoincrement not null,
                        path text not null,
                        mtime integer not null,
                        size integer not null,
                        uid integer not null,
                        gid integer not null,
                        mode integer not null,
                        filetype integer not null,
                    );
            "#)?;
            {
                let mut stmt = tx.prepare("insert into new_file (path,mtime,size,uid,gid,mode,filetype) values (?1,?2,?3,?4,?5,?6,?7)")?;
                // use the glob patterns to produce a list of files
                for g in &self.patterns {
                    'ENTRY:
                    for entry in glob::glob(g.as_str())? {
                        let entry = entry?;
                        // ignore any entries that are not one of: directory, file, symlink
                        if !entry.is_dir() && !entry.is_file() && !entry.is_symlink() {
                            continue;
                        }
                        // check include filters
                        if !self.include.is_empty() {
                            let mut to_include = false;
                            for i in &self.include {
                                if i.is_match(entry.as_os_str().as_bytes()) {
                                    to_include = true;
                                    break;
                                }
                            }
                            if !to_include {
                                continue 'ENTRY;
                            }
                        }
                        // check exclude filters
                        if !self.exclude.is_empty() {
                            for e in &self.exclude {
                                if e.is_match(entry.as_os_str().as_bytes()) {
                                    continue 'ENTRY;
                                }
                            }
                        }
                        let attr = std::fs::metadata(&entry)?;
                        stmt.execute(params![
                            stfu8::encode_u8_pretty(entry.as_os_str().as_bytes()),
                            attr.mtime(),
                            attr.size(),
                            attr.uid(),
                            attr.gid(),
                            attr.mode(),
                            (if entry.is_dir() {FileType::Directory} else if entry.is_symlink() {FileType::Symlink} else {FileType::File}) as u8,
                        ])?;
                    }
                }
            }
            {
                // get the complete list of backup diffs from the db
                let mut stmt = tx.prepare(r#"
                with recursive parent_backup(id, date_created, uuid, parent_backup_id, depth) as (
                    select id, date_created, uuid, parent_backup_id, 0 as depth
                    from backup
                    where is_complete and id = ?1
                    union all
                    select id, date_created, uuid, parent_backup_id, pb.depth+1 as depth
                    from backup b
                    join parent_backup pb 
                      on pb.parent_backup_id = b.id
                    where b.is_complete) 
                with full_backup(backup_id, uid, gid, mode, is_deleted, file_id, tape_id, path, mtime, size, filetype) as materialized (
                select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype
                from (parent_backup pb
                    join backup2file b2f
                    on b2f.parent_backup_id = pb.id
                    join file f
                    on b2f.file_id=f.id)
                join (parent_backup pb2
                    join backup2file b2f2
                    on b2f2.parent_backup_id=pb2.id
                    join file f2
                    on b2f2.file_id=f2.id)
                on f.path=f2.path
                    and f.mtime=f2.mtime
                    and f.size=f2.size
                    and f.filetype=f2.filetype
                    and b2f.depth <= b2f2.depth
                group by 1,2,3,4,5,6,7,8,9,10,11
                having count(*)=1)
                select *
                from new_file nf
                full join full_backup fb
                on fb.path=nf.path
                and fb.mtime=nf.mtime
                and fb.size=nf.size
                and fb.filetype=nf.filetype
                where ((fb.path is null or fb.is_deleted) and nf.path is not null)
                or (fb.path is not null and nf.path is null)
                order by nf.path nulls last, fb.path nulls last
                "#)?;
                let mut rows = stmt.query([parent_backup_id])?;
                while let Some(row) = rows.next()? {
                    let path: Option<String> = row.get(1)?;
                    let path2: Option<String> = row.get(15)?;
                    let filetype: u8 = row.get(6)?;
                    let filetype2: u8 = row.get(17)?;
                    let backup_diff = BackupDiff{
                        new_file: if let Some(path) = path{Some(NewFile{
                            id: row.get(0)?,
                            path: PathBuf::from(OsString::from_vec(stfu8::decode_u8(&path)?)),
                            mtime: row.get(2)?,
                            size: row.get(3)?,
                            uid: row.get(4)?,
                            gid: row.get(5)?,
                            mode: row.get(6)?,
                            filetype: FileType::try_from(filetype)?,
                        })} else {None},
                        full_backup: if let Some(path2) = path2 {Some(FullBackup {
                            backup_id: row.get(8)?,
                            uid: row.get(9)?,
                            gid: row.get(10)?,
                            mode: row.get(11)?,
                            is_deleted: row.get(12)?,
                            file_id: row.get(13)?,
                            tape_id: row.get(14)?,
                            path: PathBuf::from(OsString::from_vec(stfu8::decode_u8(&path2)?)),
                            mtime: row.get(16)?,
                            size: row.get(17)?,
                            filetype: FileType::try_from(filetype2)?,
                        })} else {None},
                    };
                    if let Some(new_file) = &backup_diff.new_file {
                        if backup_diff.full_backup.is_none() {
                            to_copy.insert(new_file.path.clone(), new_file.clone());
                        }
                    }
                    backup_diffs.push(backup_diff);
                }
            }
            tx.rollback()?;
        }

        if let Some(backup) = &backup {
            {
                let tx = conn.transaction()?;
                {
                    let mut b2f_stmt = tx.prepare(r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (?1,?2,?3,?4,?5,?6)"#)?;
                    for backup_diff in &backup_diffs {
                        if let Some(full_backup) = &backup_diff.full_backup {
                            // creating a new db record using already backed up file
                            // don't need to transfer anything
                            if let Some(new_file) = &backup_diff.new_file {
                                let is_deleted = false;
                                b2f_stmt.execute(params![
                                    &backup.id,
                                    full_backup.file_id,
                                    new_file.uid,
                                    new_file.gid,
                                    new_file.mode,
                                    is_deleted,
                                ])?;
                            }
                            // create a deletion record
                            else {
                                let is_deleted = true;
                                b2f_stmt.execute(params![
                                    &backup.id,
                                    full_backup.file_id,
                                    full_backup.uid,
                                    full_backup.gid,
                                    full_backup.mode,
                                    is_deleted,
                                ])?;
                            }
                        }
                    }
                }
                tx.commit()?;
            }

            // now transfer the files
            if ! to_copy.is_empty() {
                let date_created = backup.date_created;
                let naive = NaiveDateTime::from_timestamp(date_created, 0);
                let datetime: DateTime<Utc> = DateTime::from_utc(naive, Utc);
                let newdate = datetime.format("%Y-%m-%d %H:%M:%S");
                let timestamp = format!("{newdate}");

                let dest = [
                    &self.ltfs_path, 
                    &PathBuf::from("backup"),
                    &PathBuf::from(
                        self.db.file_stem().unwrap_or(&OsString::from("backup"))),
                    &PathBuf::from(timestamp),
                    &PathBuf::from(&backup.uuid),
                ].iter().collect::<PathBuf>();

                let mut verified_files = Vec::<PathBuf>::new();
                while ! to_copy.is_empty() {
                    let tape = self.ready_storage()?;
                    let mut tape_full = false;
                    let mut old_dir = Option::<(PathBuf, i64)>::None;
                    for (_, file) in &to_copy {
                        if let Some(file_name) = file.path.file_name() {
                            let dest_path = [
                                &dest.as_path(), 
                                &file.path.parent().unwrap_or(PathBuf::from(".").as_path()),
                                ].iter().collect::<PathBuf>();
                            // check if we need to set old_dir's mtime
                            if let Some(old_dir) = &old_dir {
                                if dest_path != old_dir.0 {
                                    let mtime = FileTime::from_unix_time(old_dir.1, 0);
                                    if let Err(e) = filetime::set_file_times(&old_dir.0, mtime.clone(), mtime.clone()) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                }
                            }
                            // handle file types
                            match file.filetype {
                                FileType::Directory => {
                                    let destdir = [
                                        &dest,
                                        &file.path,
                                    ].iter().collect::<PathBuf>();
                                    // create the directory
                                    if let Err(e) = std::fs::create_dir(&destdir) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                    // set old_dir so we can set the mtime later
                                    old_dir = Some((destdir.clone(), file.mtime));
                                },
                                FileType::Symlink => {
                                    // create the symlink
                                    let dest_file = [&dest_path, &PathBuf::from(file_name)].iter().collect::<PathBuf>();
                                    if let Err(e) = std::os::unix::fs::symlink(std::fs::read_link(&file.path)?, &dest_file) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                    // set the symlinks's mtime
                                    let mtime = FileTime::from_unix_time(file.mtime, 0);
                                    if let Err(e) = filetime::set_symlink_file_times(&dest_file, mtime.clone(), mtime.clone()) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                },
                                FileType::File => {
                                    // copy file to temp path first
                                    let randext = rand::thread_rng()
                                        .sample_iter(&Alphanumeric)
                                        .take(10)
                                        .map(char::from)
                                        .collect::<String>();
                                    let mut tmp_file_name = Vec::<u8>::new();
                                    tmp_file_name.extend(b".");
                                    tmp_file_name.extend(file_name.as_bytes());
                                    tmp_file_name.extend(b".");
                                    tmp_file_name.extend(randext.as_bytes());
                                    let tmp_file_name = PathBuf::from(OsString::from_vec(tmp_file_name));
                                    let tmp_file = [&dest_path, &tmp_file_name].iter().collect::<PathBuf>();
                                    let dest_file = [&dest_path, &PathBuf::from(file_name)].iter().collect::<PathBuf>();
                                    if let Err(e) = std::fs::copy(&file.path, &tmp_file) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                    // set the file's mtime
                                    let mtime = FileTime::from_unix_time(file.mtime, 0);
                                    if let Err(e) = filetime::set_file_times( &tmp_file, mtime.clone(), mtime.clone()) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                    // then move file into place
                                    if let Err(e) = std::fs::rename(&tmp_file, dest_file) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                },
                            };
                        }
                    }
                    // check if we need to set old_dir's mtime
                    if let Some(old_dir) = old_dir {
                        let mtime = FileTime::from_unix_time(old_dir.1, 0);
                        if let Err(e) = filetime::set_file_times(&old_dir.0, mtime.clone(), mtime.clone()) {
                            if self.is_tape_full()? {
                                tape_full = true;
                            }
                            else {
                                Err(e)?
                            }
                        }
                    }

                    // if the transfer completed without filling up the tape, then 
                    // we can assume all the transferred files copied successfully
                    if ! tape_full {
                        verified_files = to_copy.keys().map(|k| k.clone()).collect();
                        to_copy.clear();
                    }
                    // otherwise, the tape has become full. 
                    // we need to re-mount the filesystem to ensure we're using the most 
                    // recent index that was able to be written. Then we need to
                    // scan the ltfs filesystem to verify which files actually
                    // were transferred successfully.
                    else {
                        self.unmount_tape()?;
                        self.mount_tape()?;

                        verified_files.clear();
                        for entry in walkdir::WalkDir::new(&dest) {
                            let entry = entry?;
                            let path = entry.path();
                            let orig_path = [
                                &PathBuf::from("/"),
                                &PathBuf::from(path.strip_prefix(&dest)?),
                            ].iter().collect::<PathBuf>();
                            if let Some(p) = to_copy.get(&orig_path) {
                                verified_files.push(p.path.clone());
                            }
                        }
                    }
                    if ! verified_files.is_empty() {
                        let tx = conn.transaction()?;
                        {
                            let mut file_stmt = tx.prepare(r#"insert into file (tape_id,path,mtime,size) values (?1,?2,?3,?4) returning *"#)?;
                            let mut b2f_stmt = tx.prepare(r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (?1,?2,?3,?4,?5,?6)"#)?;
                            let label = tape.label.ok_or_else(|| eyre!("No label found for tape {tape}", tape=tape.index))?.clone();
                            let tape_record = tapes.get(&label).ok_or_else(|| eyre!("No tape record found for {label}"))?;
                            for verified_file in &verified_files {
                                // remove any files from to_copy that have been verified as being copied already
                                if let Some(verified_file) = to_copy.remove(verified_file) {
                                    // write file and backup2file db records
                                    let mut rows = file_stmt.query(params![
                                        &tape_record.id, 
                                        stfu8::encode_u8_pretty(verified_file.path.as_os_str().as_bytes()),
                                        &verified_file.mtime,
                                        &verified_file.size,
                                    ])?;
                                    if let Some(row) = rows.next()? {
                                        let file_id: i64 = row.get(0)?;
                                        let is_deleted = false;
                                        b2f_stmt.execute(params![
                                            &backup.id,
                                            file_id,
                                            &verified_file.uid,
                                            &verified_file.gid,
                                            &verified_file.mode,
                                            is_deleted,
                                        ])?;
                                    }
                                }
                            }
                        }
                        tx.commit()?;
                        verified_files.clear();
                    }
                }
            }
            // set the backup complete flag in the db
            conn.execute(r#"update backup set is_complete=true where backup_id=?1"#, params![&backup.id])?;
        }
        else {
            Err(eyre!("No backup found"))?;
        }
        Ok(())
    }

    fn restore_files(&self) -> Result<()> {
        // initialize the backup set's sqlite database
        self.init_db()?;
        let conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<i64,Tape>::new();
        {
            let mut stmt = conn.prepare("select * from tape order by id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                tapes.insert(id, Tape{
                    id: id,
                    label: row.get(1)?,
                    is_full: row.get(2)?,
                    date_added: row.get(3)?,
                });
            }
        }

        // get the selected (or most recent if unspecified) backup record
        let mut backup = Option::<Backup>::None;
        {
            let sql = format!(r#"
            select * 
            from backup 
            where is_complete 
              {where}
            order by date_created desc nulls last limit 1
            "#, where=if self.backup_id.is_some() || self.backup_uuid.is_some() || self.backup_date.is_some() 
                {r#"and (id=?1 or uuid=?2 or date_created <= ?3)"#} 
                else {""});
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = if self.backup_id.is_some() || self.backup_uuid.is_some() || self.backup_date.is_some() {
                let backup_date = self.backup_date.clone().map(|bd| 
                    bd.parse::<dateparser::DateTimeUtc>().ok().map(|d| 
                        d.0.timestamp())
                    ).unwrap_or(None);
                stmt.query(params![
                    self.backup_id,
                    self.backup_uuid,
                    &backup_date,
                ])?
            } else {
                stmt.query(params![])?
            };
            if let Some(row) = rows.next()? {
                backup = Some(Backup {
                    id: row.get(0)?,
                    date_created: row.get(1)?,
                    uuid: row.get(2)?,
                    parent_backup_id: row.get(3)?,
                    is_complete: row.get(4)?,
                });
            }
        }

        if let Some(backup) = backup {
            // get the complete list of backup records from the db
            let mut stmt = conn.prepare(r#"
            with recursive parent_backup(id, date_created, uuid, parent_backup_id, depth) as (
                select id, date_created, uuid, parent_backup_id, 0 as depth
                from backup
                where is_complete and id = ?1
                union all
                select id, date_created, uuid, parent_backup_id, pb.depth+1 as depth
                from backup b
                join parent_backup pb 
                    on pb.parent_backup_id = b.id
                where b.is_complete) 
            select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype
            from (parent_backup pb
                join backup2file b2f
                on b2f.parent_backup_id = pb.id
                join file f
                on b2f.file_id=f.id)
            join (parent_backup pb2
                join backup2file b2f2
                on b2f2.parent_backup_id=pb2.id
                join file f2
                on b2f2.file_id=f2.id)
            on f.path=f2.path
                and f.mtime=f2.mtime
                and f.size=f2.size
                and f.filetype=f2.filetype
                and b2f.depth <= b2f2.depth
            group by 1,2,3,4,5,6,7,8,9,10,11
            having count(*)=1
            order by f.tape_id nulls last, f.path nulls last
            "#)?;
            let mut old_tape_id = Option::<i64>::None;
            let mut rows = stmt.query([&backup.id])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(7)?;
                let filetype: u8 = row.get(10)?;
                let full_backup = FullBackup {
                    backup_id: row.get(0)?,
                    uid: row.get(1)?,
                    gid: row.get(2)?,
                    mode: row.get(3)?,
                    is_deleted: row.get(4)?,
                    file_id: row.get(5)?,
                    tape_id: row.get(6)?,
                    path: PathBuf::from(OsString::from_vec(stfu8::decode_u8(&path)?)),
                    mtime: row.get(8)?,
                    size: row.get(9)?,
                    filetype: FileType::try_from(filetype)?,
                };
            }

        }
        else {
            Err(eyre!("No backup found matching backup_id {backup_id:?}", backup_id=self.backup_id))?
        }
        Ok(())
    }

}

fn main() -> Result<()> {
    std::env::set_var("RUST_BACKTRACE", "full");
    std::env::set_var("RUST_LIB_BACKTRACE", "1");

    let mut options = Options::from_args();

    if let Err(e) = which(&options.mtx) {
        Err(eyre!("Could not find mtx program: {mtx}: {e}", mtx=options.mtx.display()))?
    }
    if let Err(e) = which(&options.mt) {
        Err(eyre!("Could not find mt program: {mt}: {e}", mt=options.mt.display()))?
    }
    if let Err(e) = which(&options.ltfs) {
        Err(eyre!("Could not find ltfs program: {ltfs}: {e}", ltfs=options.ltfs.display()))?
    }
    if let Err(e) = which(&options.mkltfs) {
        Err(eyre!("Could not find mkltfs program: {mkltfs}: {e}", mkltfs=options.mkltfs.display()))?
    }
    if let Err(e) = which(&options.umount) {
        Err(eyre!("Could not find umount program: {umount}: {e}", umount=options.umount.display()))?
    }
    if ! options.drive.exists() {
        Err(eyre!("Could not find drive device: {drive}", drive=options.drive.display()))?
    }
    if ! options.loader.exists() {
        Err(eyre!("Could not find loader device: {loader}", loader=options.loader.display()))?
    }
    if ! options.db.is_file() && !options.create_db {
        Err(eyre!("Could not find metadata database: {db}", db=options.db.display()))?
    }
    if let Some(tapes_db) = &options.tapes_db {
        if ! tapes_db.is_file() && !options.create_db {
            Err(eyre!("Could not find tapes metadata database: {tapes_db}", tapes_db=tapes_db.display()))?
        }
    }
    else {
        let mut tapes_db = options.db.clone();
        tapes_db.set_file_name("tapes.db");
        if ! tapes_db.is_file() && !options.create_db {
            Err(eyre!("Could not find tapes metadata database: {tapes_db}", tapes_db=tapes_db.display()))?
        }
        options.tapes_db = Some(tapes_db);
    }
    if ! options.ltfs_path.is_dir() {
        Err(eyre!("Could not find LTFS mount directory:: {ltfs_path}", ltfs_path=options.ltfs_path.display()))?
    }
    if ! options.restore_path.is_dir() && options.restore {
        Err(eyre!("Could not find restore path:: {restore_path}", restore_path=options.restore_path.display()))?
    }

    if options.restore && !options.backup{
        options.restore_files()?;
    }
    else if options.backup && !options.restore {
        options.backup_files()?;
    }
    else {
        eprintln!("Must specify one of: --backup, --restore");
        std::process::exit(1);
    }

    Ok(())
}
