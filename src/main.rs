#![cfg_attr(feature = "cargo-clippy", allow(cyclomatic_complexity, trivial_regex))]
use std::os::unix::prelude::MetadataExt;
use std::path::PathBuf;
use std::io::{BufRead, BufReader};
use std::collections::{HashSet, LinkedList};
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, NaiveDateTime, Utc};
use num_enum::{TryFromPrimitive, IntoPrimitive};
use regex::Regex;
use glob::Pattern;

use serde::{Serializer, Deserializer, Deserialize, de};
use structopt::StructOpt;
use walkdir::WalkDir;
use std::vec::Vec;

use lazy_regex::{regex_captures, regex_is_match, regex_replace};

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
use nix::sys::stat::Mode;
use nix::unistd::{Uid,Gid};
use users::{Users, Groups};

use log::{info, debug, error, warn, trace};
use flexi_logger::Logger;
use serde_derive::{Deserialize, Serialize};

// /dev/sg0 lto5 drive
// /dev/sg1 lto5 loader
// /dev/sg2 lto8 drive
// /dev/sg4 lto8 loader

#[derive(StructOpt, Debug)]
struct BackupCmd {
    #[structopt(long = "include", help = "Regex patterns to include files", name="INCLUDE_PATTERN")]
    include: Vec<Pattern>,
    #[structopt(long = "exclude", help = "Regex patterns to exclude files", name="EXCLUDE_PATTERN")]
    exclude: Vec<Pattern>,
    #[structopt(long = "include-regex", help = "Regex patterns to include files", name="INCLUDE_REGEX")]
    include_regex: Vec<regex::bytes::Regex>,
    #[structopt(long = "exclude-regex", help = "Regex patterns to exclude files", name="EXCLUDE_REGEX")]
    exclude_regex: Vec<regex::bytes::Regex>,
    #[structopt(help = "Glob patterns to match files in filesystem", name="GLOB_PATTERN")]
    patterns: Vec<Pattern>,
    #[structopt(long = "dry-run", help = "Do not actually backup any files, just show what files would be backed up")]
    dry_run: bool,
}

#[derive(StructOpt, Debug)]
struct RestoreCmd {
    #[structopt(long = "restore-path", help = "Restore path", name="RESTORE_PATH", default_value=".")]
    restore_path: PathBuf,
    #[structopt(long = "noclobber", help = "Do not ovewrite existing files during a restore")]
    noclobber: bool,
    #[structopt(long = "backup-id", help = "Backup ID to use for restore (default is latest backup)", name="BACKUP_ID")]
    backup_id: Option<i64>,
    #[structopt(long = "backup-uuid", help = "Backup UUID to use for restore (default is latest backup)", name="BACKUP_UUID")]
    backup_uuid: Option<String>,
    #[structopt(long = "backup-date", help = "Backup date to use for restore (default is latest backup)", name="BACKUP_DATETIME")]
    backup_date: Option<String>,
    #[structopt(long = "include", help = "Regex patterns to include files", name="INCLUDE_PATTERN")]
    include: Vec<Pattern>,
    #[structopt(long = "exclude", help = "Regex patterns to exclude files", name="EXCLUDE_PATTERN")]
    exclude: Vec<Pattern>,
    #[structopt(long = "include-regex", help = "Regex patterns to include files", name="INCLUDE_REGEX")]
    include_regex: Vec<regex::bytes::Regex>,
    #[structopt(long = "exclude-regex", help = "Regex patterns to exclude files", name="EXCLUDE_REGEX")]
    exclude_regex: Vec<regex::bytes::Regex>,
    #[structopt(help = "Glob patterns to filter restore files", name="PATTERN")]
    patterns: Vec<Pattern>,
    #[structopt(long = "dry-run", help = "Do not actually restore any files, just show what files would be restored")]
    dry_run: bool,
    #[structopt(long = "complete", help = "Only restore from complete backups")]
    complete: bool,
}

#[derive(StructOpt, Debug)]
struct ListBackupsCmd { 
    #[structopt(long = "backup-id", help = "Backup ID to search", name="BACKUP_ID")]
    backup_id: Option<i64>,
    #[structopt(long = "backup-uuid", help = "Backup UUID to search", name="BACKUP_UUID")]
    backup_uuid: Option<String>,
    #[structopt(long = "backup-date", help = "Backup date to search", name="BACKUP_DATETIME")]
    backup_date: Option<String>,
    #[structopt(long = "complete", help = "Only show complete backups")]
    complete: bool,
}

#[derive(StructOpt, Debug)]
struct ListFilesCmd {
    #[structopt(long = "backup-id", help = "Backup ID to search", name="BACKUP_ID")]
    backup_id: Option<i64>,
    #[structopt(long = "backup-uuid", help = "Backup UUID to search", name="BACKUP_UUID")]
    backup_uuid: Option<String>,
    #[structopt(long = "backup-date", help = "Backup date to search", name="BACKUP_DATETIME")]
    backup_date: Option<String>,
    #[structopt(long = "include", help = "Regex patterns to include files", name="INCLUDE_PATTERN")]
    include: Vec<Pattern>,
    #[structopt(long = "exclude", help = "Regex patterns to exclude files", name="EXCLUDE_PATTERN")]
    exclude: Vec<Pattern>,
    #[structopt(long = "include-regex", help = "Regex patterns to include files", name="INCLUDE_REGEX")]
    include_regex: Vec<regex::bytes::Regex>,
    #[structopt(long = "exclude-regex", help = "Regex patterns to exclude files", name="EXCLUDE_REGEX")]
    exclude_regex: Vec<regex::bytes::Regex>,
    #[structopt(help = "Glob patterns to filter search results", name="GLOB_PATTERN")]
    patterns: Vec<Pattern>,
}

#[derive(StructOpt, Debug)]
enum Command {
    Backup(BackupCmd),
    Restore(RestoreCmd),
    ListBackups(ListBackupsCmd),
    ListFiles(ListFilesCmd),
}

#[derive(StructOpt, Debug)]
#[structopt(name = "backup2tape", about = "Incremental tape archiving tool", global_settings = &[clap::AppSettings::ColoredHelp, clap::AppSettings::ArgRequiredElseHelp, clap::AppSettings::UnifiedHelpMessage])]
struct Options {
    //#[structopt(long = "drive", help = "Path to tape drive device", name="DEVICE", default_value="/dev/tape/by-id/scsi-35000e111c89b5001-nst")]
    #[structopt(long = "drive", help = "Path to tape drive device", name="DRIVE_DEVICE", default_value="/dev/sg2")]
    drive: PathBuf,
    #[structopt(long = "loader", help = "Path to loader device", name="LOADER_DEVICE", default_value="/dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0")]
    loader: PathBuf,
    #[structopt(long = "mtx", help = "Path to mtx command (from mtx package)", name="MTX_PATH", default_value="mtx")]
    mtx: PathBuf,
    #[structopt(long = "mt", help = "Path to mt command (from cpio package)", name="MT_PATH", default_value="mt")]
    mt: PathBuf,
    #[structopt(long = "ltfs", help = "Path to ltfs command", name="LTFS_PATH", default_value="/usr/local/bin/ltfs")]
    ltfs: PathBuf,
    #[structopt(long = "mkltfs", help = "Path to mkltfs command", name="MKLTFS_PATH", default_value="/usr/local/bin/mkltfs")]
    mkltfs: PathBuf,
    #[structopt(long = "umount", help = "Path to umount command", name="UMOUNT_PATH", default_value="umount")]
    umount: PathBuf,
    #[structopt(long = "db", help = "Path to SQLite backup metadata database", name="DB_PATH")]
    db: PathBuf,
    #[structopt(long = "tapes-db", help = "Path to SQLite tapes metadata database", name="TAPES_DB_PATH")]
    tapes_db: Option<PathBuf>,
    #[structopt(long = "create-db", help = "Create the metadata database if it doesn't exist")]
    create_db: bool,
    #[structopt(long = "ltfs-path", help = "Mount path for LTFS filesystem", name="LTFS_MOUNT_PATH", default_value="/media/lto8")]
    ltfs_path: PathBuf,
    #[structopt(long = "tape", help = "List of tapes to use for backup and restore. If none specified, use any tapes", name="TAPE_LABEL")]
    tapes: Vec<String>,
    #[structopt(long = "ignore-tape", help = "Regex patterns to ignore tapes with matching label", name="IGNORE_TAPE_REGEX", default_value="^CLN")]
    ignore_tape: Regex,
    #[structopt(long = "verbose", help = "Run in debug logging mode")]
    verbose: bool,
    #[structopt(long = "trace", help = "Run in trace logging mode")]
    trace: bool,
    #[structopt(subcommand)]
    cmd: Option<Command>,
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

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct StorageElement {
    index: usize,
    label: Option<String>,
    storage_type: StorageType,
    status: StorageStatus,
    storage_element_index: Option<usize>,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct Tape {
    id: i64,
    label: String,
    is_full: bool,
    date_added: String,
}

#[derive(PartialEq, Eq, Copy, Clone, Debug, PartialOrd, Ord, IntoPrimitive, TryFromPrimitive, Serialize, Deserialize)]
#[repr(u8)]
enum FileType {
    File = 0,
    Directory = 1,
    Symlink = 2,
}

fn pathbuf_serialize<S>(x: &PathBuf, s: S) -> Result<S::Ok, S::Error> where S: Serializer {
    s.serialize_str(&stfu8::encode_u8_pretty(x.as_os_str().as_bytes()))
}

fn pathbuf_deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
where D: Deserializer<'de> {
    let s: String = Deserialize::deserialize(deserializer)?;
    Ok(PathBuf::from(OsString::from_vec(stfu8::decode_u8(&s).map_err(de::Error::custom)?)))
}

fn option_pathbuf_serialize<S>(x: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> where S: Serializer {
    match x {
        Some(x) => s.serialize_some(&stfu8::encode_u8_pretty(x.as_os_str().as_bytes())),
        None => s.serialize_none(),
    }
}

fn option_pathbuf_deserialize<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where D: Deserializer<'de> {
    let s: Option<String> = Deserialize::deserialize(deserializer)?;
    match s {
        Some(s) => Ok(Some(PathBuf::from(OsString::from_vec(stfu8::decode_u8(&s).map_err(de::Error::custom)?)))),
        None => Ok(None),
    }
}

fn filetype_serialize<S>(x: &FileType, s: S) -> Result<S::Ok, S::Error> where S: Serializer {
    s.serialize_u8(*x as u8)
}

fn filetype_deserialize<'de, D>(deserializer: D) -> Result<FileType, D::Error>
where D: Deserializer<'de> {
    let i: u8 = Deserialize::deserialize(deserializer)?;
    Ok(FileType::try_from(i).map_err(de::Error::custom)?)
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct File {
    id: i64,
    tape_id: i64,
    #[serde(serialize_with = "pathbuf_serialize")]
    #[serde(deserialize_with = "pathbuf_deserialize")]
    path: PathBuf,
    mtime: i64,
    size: u64,
    #[serde(serialize_with = "filetype_serialize")]
    #[serde(deserialize_with = "filetype_deserialize")]
    filetype: FileType,
    #[serde(serialize_with = "option_pathbuf_serialize")]
    #[serde(deserialize_with = "option_pathbuf_deserialize")]
    symlink_target: Option<PathBuf>,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct Backup {
    id: i64,
    date_created: i64,
    uuid: String,
    parent_backup_id: Option<i64>,
    is_complete: bool,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct Backup2File {
    backup_id: i64,
    file_id: i64,
    uid: u32,
    gid: u32,
    mode: u32,
    is_deleted: bool,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct NewFile {
    id: i64,
    #[serde(serialize_with = "pathbuf_serialize")]
    #[serde(deserialize_with = "pathbuf_deserialize")]
    path: PathBuf,
    mtime: i64,
    size: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    #[serde(serialize_with = "filetype_serialize")]
    #[serde(deserialize_with = "filetype_deserialize")]
    filetype: FileType,
    #[serde(serialize_with = "option_pathbuf_serialize")]
    #[serde(deserialize_with = "option_pathbuf_deserialize")]
    symlink_target: Option<PathBuf>,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct FullBackup {
    backup_id: i64,
    uid: u32,
    gid: u32,
    mode: u32,
    is_deleted: bool,
    file_id: i64,
    tape_id: i64,
    #[serde(serialize_with = "pathbuf_serialize")]
    #[serde(deserialize_with = "pathbuf_deserialize")]
    path: PathBuf,
    mtime: i64,
    size: u64,
    #[serde(serialize_with = "filetype_serialize")]
    #[serde(deserialize_with = "filetype_deserialize")]
    filetype: FileType,
    #[serde(serialize_with = "option_pathbuf_serialize")]
    #[serde(deserialize_with = "option_pathbuf_deserialize")]
    symlink_target: Option<PathBuf>,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord)]
struct BackupDiff {
    new_file: Option<NewFile>,
    full_backup: Option<FullBackup>,
}

macro_rules! cmd {
    ( $program:expr $(, $arg:expr )* $(,)? ) => {
        {
            use std::ffi::OsString;
            use duct::IntoExecutablePath;
            let args: std::vec::Vec<OsString> = std::vec![$( Into::<OsString>::into($arg) ),*];
            let args_string: Vec<_> = args.iter().map(|a| shell_escape::unix::escape((*a).to_string_lossy())).collect();
            debug!("Running command: {} {}", 
                shell_escape::unix::escape($program.to_executable().to_string_lossy()), 
                args_string.join(" "));
            duct::cmd($program, args)
        }
    };
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
        debug!("Running loader_status");
        let mut elements = Vec::<StorageElement>::new();
        // mtx -f /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 status
        debug!("Running mtx status command");
        let mtx_cmd = cmd!(&self.mtx, "-f", &self.loader, "status");
        let reader = mtx_cmd.reader()?;
        let reader = BufReader::new(reader);

        for line in reader.lines() {
            let line = line?;
            debug!("mtx line: {line}");
            if let Some((_, drive_num)) = regex_captures!(r#"^\s*Data Transfer Element (\d+):Empty$"#, &line) {
                let element = StorageElement {
                    index: drive_num.parse::<usize>()?,
                    label: None,
                    storage_type: StorageType::Drive,
                    status: StorageStatus::Empty,
                    storage_element_index: None,
                };
                debug!("{element:#?}");
                elements.push(element);
            }
            else if let Some((_, drive_num, storage_element_index, _, label)) = regex_captures!(r#"^\s*Data Transfer Element (\d+):Full \(Storage Element (\d+) Loaded\)(:VolumeTag =\s*(.+))?$"#, &line) {
                let element = StorageElement {
                    index: drive_num.parse::<usize>()?,
                    label: Some(label.trim_end().to_string()),
                    storage_type: StorageType::Drive,
                    status: StorageStatus::Full,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                };
                debug!("{element:#?}");
                elements.push(element);
            }
            else if let Some((_, storage_element_index,)) = regex_captures!(r#"^\s*Storage Element (\d+):Empty$"#, &line) {
                let element = StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: None,
                    storage_type: StorageType::Element,
                    status: StorageStatus::Empty,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                };
                debug!("{element:#?}");
                elements.push(element);
            }
            else if let Some((_, storage_element_index, _, label)) = regex_captures!(r#"^\s*Storage Element (\d+):Full( :VolumeTag=(.+))?$"#, &line) {
                let element = StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: Some(label.trim_end().to_string()),
                    storage_type: StorageType::Element,
                    status: StorageStatus::Full,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                };
                debug!("{element:#?}");
                elements.push(element);
            }
            else if let Some((_, storage_element_index,)) = regex_captures!(r#"^\s*Storage Element (\d+) IMPORT.EXPORT:Empty$"#, &line) {
                let element = StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: None,
                    storage_type: StorageType::ImportExport,
                    status: StorageStatus::Empty,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                };
                debug!("{element:#?}");
                elements.push(element);
            }
            else if let Some((_, storage_element_index, _, label)) = regex_captures!(r#"^\s*Storage Element (\d+) IMPORT.EXPORT:Full( :VolumeTag=(.+))?$"#, &line) {
                let element = StorageElement {
                    index: storage_element_index.parse::<usize>()?,
                    label: Some(label.trim_end().to_string()),
                    storage_type: StorageType::ImportExport,
                    status: StorageStatus::Full,
                    storage_element_index: Some(storage_element_index.parse::<usize>()?),
                };
                debug!("{element:#?}");
                elements.push(element);
            }
        }
        debug!("elements: {elements:#?}");
        Ok(elements)
    }

    fn is_mounted(&self) -> Result<bool> {
        debug!("Running is_mounted");
        let mut system = sysinfo::System::new();
        system.refresh_disks_list();
        for disk in system.disks() {
            debug!("Checking disk {disk} against {ltfs_path}", 
                disk=disk.mount_point().to_string_lossy(),
                ltfs_path=self.ltfs_path.to_string_lossy());
            if disk.mount_point() == self.ltfs_path {
                debug!("Found match");
                return Ok(true);
            }
        }
        Err(eyre!("No disk matching {ltfs_path} was found!", ltfs_path=self.ltfs_path.display()))
    }

    fn is_tape_full(&self) -> Result<bool> {
        debug!("Running is_tape_full");
        let mut system = sysinfo::System::new();
        system.refresh_disks_list();
        for disk in system.disks_mut() {
            debug!("Checking disk {disk} against {ltfs_path}", 
                disk=disk.mount_point().display(),
                ltfs_path=self.ltfs_path.display());
            if disk.mount_point() == self.ltfs_path {
                debug!("Found match, available_space = {}", disk.available_space());
                return Ok(disk.available_space() == 0);
            }
        }
        Err(eyre!("No disk matching {ltfs_path} was found!", ltfs_path=self.ltfs_path.display()))
    }

    fn tape_status(&self) -> Result<Vec<Tape>> {
        debug!("Running tape_status");
        let mut tapes = Vec::<Tape>::new();
        if let Some(tapes_db) = &self.tapes_db {
            debug!("connecting to tapes_db {tapes_db}", tapes_db=tapes_db.display());
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
        debug!("Got tapes: {tapes:#?}");
        Ok(tapes)
    }

    fn insert_tape_record(&self, label: &str) -> Result<()> {
        debug!("Running insert_tape_record");
        if let Some(tapes_db) = &self.tapes_db {
            debug!("Loading tapes db {}", tapes_db.display());
            let conn = Connection::open(tapes_db)?;
            debug!("Inserting tape record {label}");
            conn.execute(r#"insert into tape (label) values (?1) on conflict(label) do nothing "#, params![label])?;
        }
        debug!("Loading metadata db {}", &self.db.display());
        let conn = Connection::open(&self.db)?;
        debug!("Inserting tape record {label}");
        conn.execute(r#"insert into tape (label) values (?1) on conflict(label) do nothing"#, params![label])?;
        Ok(())
    }

    fn set_tape_record_as_full(&self, label: &str) -> Result<()> {
        debug!("Running set_tape_record_as_full");
        if let Some(tapes_db) = &self.tapes_db {
            let conn = Connection::open(tapes_db)?;
            debug!("Setting tape {label} as full");
            conn.execute(r#"update tape set is_full=1 where label=?1"#, params![label])?;
        }
        debug!("Loading metadata db {}", &self.db.display());
        let conn = Connection::open(&self.db)?;
        debug!("Setting tape {label} as full");
        conn.execute(r#"update tape set is_full=1 where label=?1"#, params![label])?;
        Ok(())
    }

    fn wait_for_drive(&self) -> Result<()> {
        debug!("Running wait_for_drive");
        let mut drive_is_ready = false;
        for _ in 0..300 {
            debug!("Running mt status command");
            let drivestatus = cmd!("mt","-f", &self.drive, "status").run()?;
            if regex_is_match!(r#"ONLINE"#B, &drivestatus.stdout) {
                debug!("Drive is ready");
                drive_is_ready = true;
                break
            }
            debug!("Waiting 1 second...");
            std::thread::sleep(std::time::Duration::new(1, 0));
        }
        if ! drive_is_ready {
            Err(eyre!("Drive not ready!"))?
        }
        Ok(())
    }

    fn wait_for_ltfs_process_to_terminate(&self) -> Result<()> {
        debug!("Running wait_for_ltfs_process_to_terminate");
        let mut system = sysinfo::System::new();
        for _ in 0..600 {
            debug!("Refreshing processes");
            system.refresh_processes();
            let mut found_ltfs = false;
            for (_pid, process) in system.processes() {
                debug!("Checking process: {process:#?} with name \"{name}\", exe \"{exe}\", cmd \"{cmd:#?}\"",
                    name=process.name(),
                    exe=process.exe().to_string_lossy(),
                    cmd=process.cmd());
                if process.name() == "ltfs" || 
                    regex_is_match!(r#"(^|/)ltfs$"#, &process.exe().to_string_lossy()) ||
                    regex_is_match!(r#"(^|/)ltfs$"#, &process.cmd()[0])
                {
                    debug!("Found matching process");
                    found_ltfs = true;
                    break;
                }
            }
            if !found_ltfs {
                return Ok(())
            }
            debug!("Waiting 1 second...");
            std::thread::sleep(std::time::Duration::new(1, 0));
        }
        Err(eyre!("ltfs process took too long to terminate"))?
    }

    fn unmount_tape(&self) -> Result<()> {
        debug!("Running unmount_tape");
        if self.is_mounted()? {
            debug!("Running umount command");
            cmd!(&self.umount, &self.ltfs_path).run()?;
            self.wait_for_ltfs_process_to_terminate()?;
        }
        Ok(())
    }

    fn load_tape(&self, slot: usize) -> Result<()> {
        debug!("Running load_tape");
        let ls = self.loader_status()?;
        let drive_slot = ls.iter().
            filter(|se| se.storage_type==StorageType::Drive).
            next().ok_or(eyre!("Could not find drive slot"))?;
        debug!("Found drive slot: {drive_slot:#?}");

        if drive_slot.status == StorageStatus::Full {
            debug!("Drive slot is full");
            let empty_slot = ls.iter().
                filter(|se| se.storage_type==StorageType::Element && se.status==StorageStatus::Empty).
                next().ok_or(eyre!("Could not find an empty slot"))?;
            debug!("Found empty slot: {empty_slot:#?}");
            
            self.unmount_tape()?;
            debug!("Running unload command");
            cmd!(&self.mtx, "-f", &self.loader, "unload", empty_slot.index.to_string(), drive_slot.index.to_string()).run()?;
        }
        debug!("Running load commmand");
        cmd!(&self.mtx, "-f", &self.loader, "load", slot.to_string(), drive_slot.index.to_string()).run()?;
        self.wait_for_drive()?;
        Ok(())
    }

    fn mount_tape(&self) -> Result<()> {
        debug!("Running mount_tape");
        if ! self.is_mounted()? {
            debug!("Running ltfs commmand");
            cmd!(&self.ltfs, "-o", format!("devname={drive}", drive=self.drive.to_string_lossy()), &self.ltfs_path).run()?;
            if ! self.is_mounted()? {
                Err(eyre!("path {ltfs_path} was not mounted", ltfs_path=self.ltfs_path.display()))?;
            }
        }
        else {
            debug!("LTFS is already mounted");
        }
        Ok(())
    }

    fn ready_storage(&self) -> Result<StorageElement> {
        debug!("Running ready_storage");
        // get loader status
        let ls = self.loader_status()?;
        // get tape status from tapes db
        let ts: Vec<Tape> = self.tape_status()?;
        // get list of full tapes we know about
        let full_tapes = ts.iter().
            filter(|t| t.is_full).
            map(|t| t.label.clone()).collect::<HashSet<_>>();
        debug!("Got full tapes from db: {full_tapes:#?}");
        // get list of non-full tapes we know about
        let non_full_tapes = ts.iter().
            filter(|t| !t.is_full).
            map(|t| t.label.clone()).collect::<HashSet<_>>();
        debug!("Got non-full tapes from db: {non_full_tapes:#?}");

        // get list of potentially usable tapes
        let mut tapes = ls.iter().
            filter(|se| 
                (se.storage_type == StorageType::Drive || se.storage_type == StorageType::Element) && 
                se.status==StorageStatus::Full &&
                if self.tapes.is_empty() {true} else {if let Some(label) = &se.label {self.tapes.contains(label)} else {true}}).
            collect::<Vec<_>>();
        tapes.sort_by_key(|t| 
            (!(if let Some(label) = &t.label {non_full_tapes.contains(label)} else {false}), 
            &t.storage_type,
            &t.index));
        debug!("Got usable tapes: {tapes:#?}");

        // find a tape to use
        debug!("Find a tape to use");
        for tape in &tapes {
            debug!("Checking tape: {tape:#?}");
            if tape.storage_type == StorageType::Drive || tape.storage_type == StorageType::Element {
                if let Some(label) = &tape.label {
                    // if the tape is not marked as full
                    if ! full_tapes.contains(label) && ! self.ignore_tape.is_match(label) {
                        if tape.storage_type != StorageType::Drive {
                            // if ltfs filesystem is not mounted
                            if self.is_mounted()? {
                                debug!("Unmounting LTFS filesystem");
                                // attempt to mount the tape
                                self.unmount_tape()?;
                            }
                            // load the tape into the drive
                            debug!("Loading usable tape: {tape:#?}");
                            self.load_tape(tape.index)?;
                        }

                        // if no db record exists for tape, attempt to format the tape
                        // if tape is already formatted, formatting will fail
                        if !non_full_tapes.contains(label) {
                            if ! self.is_mounted()? {
                                debug!("Tape has no db record, try formatting it");
                                debug!("Running mkltfs commmand");
                                let output = cmd!(&self.mkltfs, "-d", &self.drive).unchecked().run()?;
                                match output.status.code() {
                                    Some(16) => {
                                        debug!("mkltfs returned exit code 16, already formatted");
                                    }, // already formatted
                                    Some(0) => {
                                        debug!("mkltfs format succeeded");
                                    }, // format succeeded
                                    code => Err(eyre!("mkltfs command returned exit status {code:#?}"))?,
                                }
                            }
                            // insert a db record for the tape
                            debug!("insert tape database record");
                            self.insert_tape_record(&label)?;
                        }

                        // if ltfs filesystem is not mounted
                        if !self.is_mounted()? {
                            debug!("mount the tape LTFS filesystem");
                            // attempt to mount the tape
                            self.mount_tape()?;
                        }

                        // check for available space
                        if self.is_tape_full()? {
                            debug!("LTFS filesystem is full, set tape record as full, and try another tape");
                            self.set_tape_record_as_full(label)?;
                            continue;
                        }
                        return Ok((**tape).clone())
                    }
                }
            }
        }
        Err(eyre!("Could not find any available tapes to use"))?
    }

    fn init_db(&self) -> Result<()> {
        debug!("Called init_db");
        debug!("Connecting to db {db}", db=self.db.display());
        let conn = Connection::open(&self.db)?;

        let sql = r#"
        create table if not exists tape (
            id integer primary key autoincrement not null,
            label text not null unique,
            is_full integer default false not null,
            date_added timestamp default current_timestamp not null
        );
        create table if not exists backup (
            id integer primary key autoincrement not null,
            date_created integer default (unixepoch()) not null,
            uuid text not null,
            is_complete integer not null default false,
            parent_backup_id integer references backup(id)
        );
        create table if not exists file (
            id integer primary key autoincrement not null,
            tape_id integer references tape(id),
            path text not null,
            mtime integer not null,
            size integer not null,
            filetype integer not null,
            symlink_target text
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
        "#;
        debug!("Running sql:\n{sql}\n");
        conn.execute_batch(sql)?;

        if let Some(tapes_db) = &self.tapes_db {
            debug!("Connecting to tapes db {tapes_db}", tapes_db=tapes_db.display());
            let conn = Connection::open(tapes_db)?;
            let sql = r#"
            create table if not exists tape (
                id integer primary key autoincrement not null,
                label text not null unique,
                is_full integer default false not null,
                date_added timestamp default current_timestamp not null
            )
            "#;
            debug!("Running sql:\n{sql}\n");
            conn.execute_batch(sql)?;
        }

        Ok(())
    }

    fn backup_files(&self) -> Result<()> {
        debug!("Called backup_files");
        let backup_cmd = match &self.cmd {
            Some(Command::Backup(b)) => b,
            _ => Err(eyre!("Couldn't match backup subcommand!"))?,
        };

        // initialize the backup set's sqlite database
        self.init_db()?;
        debug!("Connecting to db {db}", db=self.db.display());
        let mut conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<String,Tape>::new();
        {
            let mut stmt = conn.prepare("select * from tape")?;
            let mut rows_iter = serde_rusqlite::from_rows::<Tape>(stmt.query([])?);
            while let Some(tape) = rows_iter.next() {
                let tape = tape?;
                tapes.insert(tape.label.clone(), tape);
            }
        }
        debug!("got tapes records: {tapes:#?}");

        // get the parent backup ID
        let mut parent_backup_id = Option::<i64>::None;
        {
            let sql = r#"select id from backup order by date_created desc nulls last limit 1"#;
            debug!("Running query: {sql}");
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                parent_backup_id = Some(row.get(0)?);
            }
        }
        debug!("got parent backup id {parent_backup_id:#?}");

        // create the new backup record
        let mut backup = None;
        let backup_uuid = Uuid::new_v4();
        if ! backup_cmd.dry_run {
            let sql = r#"insert into backup (uuid,parent_backup_id) values (?1,?2) returning *"#;
            debug!("Running query: {sql}");
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query(params![backup_uuid.to_string(), parent_backup_id])?;
            let mut rows_iter = serde_rusqlite::from_rows::<Backup>(rows);
            if let Some(backup_record) = rows_iter.next() {
                let backup_record = backup_record?;
                backup = Some(backup_record);
            }
        }
        debug!("Created backup record: {backup:#?}");

        let mut to_copy = LinkedList::<NewFile>::new();

        // compute the backup diffs
        debug!("Computing backup diffs");
        let mut backup_diffs = Vec::<BackupDiff>::new();
        {
            let tx = conn.transaction()?;
            let sql =r#"
                    drop table if exists new_file;
                    create table new_file (
                        id integer primary key autoincrement not null,
                        path text unique not null,
                        mtime integer not null,
                        size integer not null,
                        uid integer not null,
                        gid integer not null,
                        mode integer not null,
                        filetype integer not null,
                        symlink_target text
                    );
            "#;
            debug!("Running sql query:\n{sql}\n");
            tx.execute_batch(sql)?;
            {
                info!("Scanning for files to backup...");
                debug!("Inserting new_file records");
                let sql = "insert into new_file (path,mtime,size,uid,gid,mode,filetype,symlink_target) values (:path,:mtime,:size,:uid,:gid,:mode,:filetype,:symlink_target) on conflict(path) do nothing";
                let mut stmt = tx.prepare(sql)?;
                let mut count = 0;
                // use the glob patterns to produce a list of files
                for g in &backup_cmd.patterns {
                    for entry in glob::glob(g.as_str())? {
                        match &entry {
                            Ok(entry) => {
                                for direntry in WalkDir::new(&entry) {
                                    match &direntry {
                                        Ok(entry) => {
                                            let path = entry.path();
                                            // ignore any entries that are not one of: directory, file, symlink
                                            if !path.is_dir() && !path.is_file() && !path.is_symlink() {
                                                continue;
                                            }
                                            // check include filters
                                            if !backup_cmd.include_regex.is_empty() {
                                                let mut to_include = false;
                                                for i in &backup_cmd.include_regex {
                                                    if i.is_match(path.as_os_str().as_bytes()) {
                                                        to_include = true;
                                                        break;
                                                    }
                                                }
                                                if !to_include {
                                                    continue;
                                                }
                                            }
                                            if !backup_cmd.include.is_empty() {
                                                let mut to_include = false;
                                                for i in &backup_cmd.include {
                                                    if i.matches_path(&path) {
                                                        to_include = true;
                                                        break;
                                                    }
                                                }
                                                if !to_include {
                                                    continue;
                                                }
                                            }
                                            // check exclude filters
                                            if !backup_cmd.exclude_regex.is_empty() {
                                                for e in &backup_cmd.exclude_regex {
                                                    if e.is_match(path.as_os_str().as_bytes()) {
                                                        continue;
                                                    }
                                                }
                                            }
                                            if !backup_cmd.exclude.is_empty() {
                                                for e in &backup_cmd.exclude {
                                                    if e.matches_path(&path) {
                                                        continue;
                                                    }
                                                }
                                            }
                                            // insert the new_file record
                                            //debug!("Found file to backup: {path}", path=path.to_string_lossy());
                                            match &entry.metadata() {
                                                Ok(attr) => {
                                                    stmt.execute(serde_rusqlite::to_params_named_with_fields(&NewFile {
                                                        id: 0,
                                                        path: PathBuf::from(path),
                                                        mtime: attr.mtime(),
                                                        size: attr.size(),
                                                        uid: attr.uid(),
                                                        gid: attr.gid(),
                                                        mode: attr.mode(),
                                                        filetype: if path.is_dir() {FileType::Directory} else if path.is_symlink() {FileType::Symlink} else {FileType::File},
                                                        symlink_target: if path.is_symlink() {Some(std::fs::read_link(&path)?)} else {None},
                                                    }, &["path","mtime","size","uid","gid","mode","filetype","symlink_target"])?.to_slice().as_slice())?;
                                                    count += 1;
                                                    if count % 1000 == 0 {
                                                        debug!("Inserted {count} records");
                                                    }
                                                },
                                                Err(e) => {
                                                    warn!("Error reading metadata for file {f}: {e}, skipping", f=entry.path().to_string_lossy())
                                                },
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Error when walking directory {d}: {e}, skipping", d=entry.to_string_lossy());
                                        },
                                    }
                                }
                            },
                            Err(e) => {
                                warn!("Error when scanning glob match: {g}: {e}, skipping", g=g.as_str());
                            },
                        }
                    }
                }
            }
            {
                // get the complete list of backup diffs from the db
                let sql = r#"
                with recursive parent_backup(id, date_created, uuid, parent_backup_id, depth) as (
                    select id, date_created, uuid, parent_backup_id, 0 as depth
                    from backup
                    where id = ?1
                    union all
                    select b.id, b.date_created, b.uuid, b.parent_backup_id, pb.depth+1 as depth
                    from backup b
                    join parent_backup pb 
                    on pb.parent_backup_id = b.id),
                full_backup(backup_id, uid, gid, mode, is_deleted, file_id, tape_id, path, mtime, size, filetype, symlink_target) as materialized (
                    select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype, f.symlink_target
                    from (parent_backup pb
                    join backup2file b2f
                        on b2f.backup_id = pb.id
                    join file f
                        on b2f.file_id=f.id)
                    join (parent_backup pb2
                    join backup2file b2f2
                        on b2f2.backup_id=pb2.id
                    join file f2
                        on b2f2.file_id=f2.id)
                    on f.path=f2.path
                    and f.mtime=f2.mtime
                    and f.size=f2.size
                    and f.filetype=f2.filetype
                    and pb.depth <= pb2.depth
                    group by 1,2,3,4,5,6,7,8,9,10,11,12
                    having count(*)=1)
                select nf.id as new_file_id,
                    nf.path as new_file_path,
                    nf.mtime as new_file_mtime,
                    nf.size as new_file_size,
                    nf.uid as new_file_uid,
                    nf.gid as new_file_gid,
                    nf.mode as new_file_mode,
                    nf.filetype as new_file_filetype,
                    nf.symlink_target as new_file_symlink_target,
                    fb.backup_id as full_backup_backup_id,
                    fb.uid as full_backup_uid,
                    fb.gid as full_backup_gid,
                    fb.mode as full_backup_mode,
                    fb.is_deleted as full_backup_is_deleted,
                    fb.file_id as full_backup_file_id,
                    fb.tape_id as full_backup_tape_id,
                    fb.path as full_backup_path,
                    fb.mtime as full_backup_mtime,
                    fb.size as full_backup_size,
                    fb.filetype as full_backup_filetype,
                    fb.symlink_target as full_backup_symlink_target
                from new_file nf
                left join full_backup fb
                    on fb.path=nf.path
                    and fb.mtime=nf.mtime
                    and fb.size=nf.size
                    and fb.filetype=nf.filetype
                where (fb.path is null or fb.is_deleted) 
                    and nf.path is not null
                union all
                select nf.id as new_file_id,
                    nf.path as new_file_path,
                    nf.mtime as new_file_mtime,
                    nf.size as new_file_size,
                    nf.uid as new_file_uid,
                    nf.gid as new_file_gid,
                    nf.mode as new_file_mode,
                    nf.filetype as new_file_filetype,
                    nf.symlink_target as new_file_symlink_target,
                    fb.backup_id as full_backup_backup_id,
                    fb.uid as full_backup_uid,
                    fb.gid as full_backup_gid,
                    fb.mode as full_backup_mode,
                    fb.is_deleted as full_backup_is_deleted,
                    fb.file_id as full_backup_file_id,
                    fb.tape_id as full_backup_tape_id,
                    fb.path as full_backup_path,
                    fb.mtime as full_backup_mtime,
                    fb.size as full_backup_size,
                    fb.filetype as full_backup_filetype,
                    fb.symlink_target as full_backup_symlink_target
                from full_backup fb
                left join new_file nf
                    on fb.path=nf.path
                    and fb.mtime=nf.mtime
                    and fb.size=nf.size
                    and fb.filetype=nf.filetype
                where fb.path is not null 
                    and nf.path is null
                order by nf.path nulls last, fb.path nulls last
                "#;
                debug!("Running sql:\n{sql}\n");
                let mut stmt = tx.prepare(sql)?;
                let mut rows = stmt.query(params![parent_backup_id])?;
                while let Some(row) = rows.next()? {
                    let backup_diff = BackupDiff {
                        new_file: if row.get::<&str,Option<String>>("new_file_path")?.is_some() {
                            Some(NewFile {
                                id: row.get("new_file_id")?,
                                path: PathBuf::from(OsString::from_vec(stfu8::decode_u8(&row.get::<&str,String>("new_file_path")?)?)),
                                mtime: row.get("new_file_mtime")?,
                                size: row.get("new_file_size")?,
                                uid: row.get("new_file_uid")?,
                                gid: row.get("new_file_gid")?,
                                mode: row.get("new_file_mode")?,
                                filetype: FileType::try_from(row.get::<&str,u8>("new_file_filetype")?)?,
                                symlink_target: if let Some(t) = row.get::<&str,Option<String>>("new_file_symlink_target")? {Some(PathBuf::from(OsString::from_vec(stfu8::decode_u8(&t)?)))} else {None},
                            })
                        } else {None},
                        full_backup: if row.get::<&str,Option<String>>("full_backup_path")?.is_some() {
                            Some(FullBackup {
                                backup_id: row.get("full_backup_backup_id")?,
                                uid: row.get("full_backup_uid")?,
                                gid: row.get("full_backup_gid")?,
                                mode: row.get("full_backup_mode")?,
                                is_deleted: row.get("full_backup_is_deleted")?,
                                file_id: row.get("full_backup_file_id")?,
                                tape_id: row.get("full_backup_tape_id")?,
                                path: PathBuf::from(OsString::from_vec(stfu8::decode_u8(&row.get::<&str,String>("full_backup_path")?)?)),
                                mtime: row.get("full_backup_mtime")?,
                                size: row.get("full_backup_size")?,
                                filetype: FileType::try_from(row.get::<&str,u8>("full_backup_filetype")?)?,
                                symlink_target: if let Some(t) = row.get::<&str,Option<String>>("full_backup_symlink_target")? {Some(PathBuf::from(OsString::from_vec(stfu8::decode_u8(&t)?)))} else {None},
                            })
                        } else {None},
                    };
                    if let Some(new_file) = &backup_diff.new_file {
                        if backup_diff.full_backup.is_none() {
                            // add new_file record to to_copy
                            to_copy.push_back(new_file.clone());
                        }
                    }
                    // add backup_diff record to backup_diffs
                    backup_diffs.push(backup_diff);
                }
            }
            tx.rollback()?;
        }
        debug!("Found {} backup_diff records, {} to_copy records", backup_diffs.len(), to_copy.len());

        if ! backup_cmd.dry_run {
            let backup_id = if let Some(backup) = &backup {Some(backup.id)} else {None};
            if let Some(backup_id) = &backup_id {
                let tx = conn.transaction()?;
                {
                    let mut count = 0;
                    let mut b2f_stmt = tx.prepare(r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (:backup_id,:file_id,:uid,:gid,:mode,:is_deleted)"#)?;
                    for backup_diff in &backup_diffs {
                        if let Some(full_backup) = &backup_diff.full_backup {
                            // creating a new db record using already backed up file
                            // don't need to transfer anything
                            if let Some(new_file) = &backup_diff.new_file {
                                b2f_stmt.execute(serde_rusqlite::to_params_named(&Backup2File {
                                    backup_id: *backup_id,
                                    file_id: full_backup.file_id,
                                    uid: new_file.uid,
                                    gid: new_file.gid,
                                    mode: new_file.mode,
                                    is_deleted: false,
                                })?.to_slice().as_slice())?;
                                count += 1;
                            }
                            // create a deletion record
                            else {
                                b2f_stmt.execute(serde_rusqlite::to_params_named(&Backup2File {
                                    backup_id: *backup_id,
                                    file_id: full_backup.file_id,
                                    uid: full_backup.uid,
                                    gid: full_backup.gid,
                                    mode: full_backup.mode,
                                    is_deleted: true,
                                })?.to_slice().as_slice())?;
                                count += 1;
                            }
                        }
                    }
                    debug!("Inserted {count} backup2file diff records");
                }
                tx.commit()?;
            }
        }

        // now transfer the files
        // produce a timestamp string
        let mut date_created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        if let Some(backup) = &backup {
            date_created = backup.date_created;
        }
        let naive = NaiveDateTime::from_timestamp(date_created, 0);
        let datetime: DateTime<Utc> = DateTime::from_utc(naive, Utc);
        let newdate = datetime.format("%Y-%m-%d_%H.%M.%S");
        let timestamp = format!("{newdate}");
        debug!("timestamp={timestamp}");

        // compute the destination path on the ltfs filesystem
        let dest = [
            &self.ltfs_path, 
            &PathBuf::from("backup"),
            &PathBuf::from(
                self.db.file_stem().unwrap_or(&OsString::from("backup"))),
            &PathBuf::from(timestamp),
            &PathBuf::from(backup_uuid.to_string()),
            &PathBuf::from("rootfs"),
        ].iter().collect::<PathBuf>();
        debug!("dest={dest}", dest=dest.to_string_lossy());

        let mut old_tape = Option::<String>::None;
        while ! to_copy.is_empty() {
            let tape = if !backup_cmd.dry_run {Some(self.ready_storage()?)} else {None};

            // update the tape db records
            {
                tapes.clear();
                let mut stmt = conn.prepare("select * from tape")?;
                let mut rows_iter = serde_rusqlite::from_rows::<Tape>(stmt.query([])?);
                while let Some(tape) = rows_iter.next() {
                    let tape = tape?;
                    tapes.insert(tape.label.clone(), tape);
                }
                debug!("updated tapes records: {tapes:#?}", tapes=tapes);
            }

            let label = if let Some(tape) = &tape {Some(tape.label.as_ref().ok_or_else(|| eyre!("No label found for tape {tape}", tape=tape.index))?)} else {None};
            let tape_record = if let Some(label) = label {Some(tapes.get(label).ok_or_else(|| eyre!("No tape record found for {label}"))?)} else {None};

            if let Some(tape) = &tape {
                if old_tape != tape.label {
                    debug!("Using tape {label:#?}", label=tape.label);
                    old_tape = tape.label.clone();
                }
            }
            let mut tape_full = false;
            let mut old_dir = Option::<(PathBuf, i64)>::None;
            while let Some(file) = to_copy.pop_front() {
                trace!("Handling file {file:#?}");
                if let Some(file_name) = file.path.file_name() {
                    let file_path = PathBuf::from(file.path.parent().unwrap_or(PathBuf::from(".").as_path()));
                    let file_path = file_path.strip_prefix("/").unwrap_or_else(|_| file_path.as_path());

                    let dest_path = PathBuf::from(stfu8::encode_u8_pretty([
                        &dest.as_path(), 
                        &file_path,
                    ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                    // make sure dest path exists
                    trace!("Create parent directories: {}", dest_path.to_string_lossy());
                    if let Err(e) = std::fs::create_dir_all(&dest_path) {
                        if self.is_tape_full()? {
                            tape_full = true;
                            break;
                        }
                        else {
                            Err(e)?
                        }
                    }
                    // check if we need to set old_dir's mtime
                    if ! backup_cmd.dry_run {
                        if let Some(old_dir) = &old_dir {
                            if dest_path != old_dir.0 {
                                trace!("old_dir changed from {old_dir:#?} to {dest_path}, setting mtime on old_dir", dest_path=dest_path.to_string_lossy());
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
                    }
                    // handle file types
                    match file.filetype {
                        FileType::Directory => {
                            trace!("is a directory");
                            let destdir = PathBuf::from(stfu8::encode_u8_pretty([
                                &dest_path,
                                &PathBuf::from(file_name),
                            ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                            trace!("destdir={destdir}", destdir=destdir.to_string_lossy());

                            info!("{path}/ -> {destdir}/", path=file.path.to_string_lossy(), destdir=destdir.to_string_lossy());
                            if ! backup_cmd.dry_run {
                                // create the directory
                                trace!("create directory {destdir}", destdir=destdir.to_string_lossy());
                                if let Err(e) = std::fs::create_dir(&destdir) {
                                    if self.is_tape_full()? {
                                        tape_full = true;
                                        break;
                                    }
                                    else {
                                        Err(e)?
                                    }
                                }
                            }
                            // set old_dir so we can set the mtime later
                            old_dir = Some((destdir.clone(), file.mtime));
                            trace!("set old_dir to {old_dir:#?}");
                        },
                        FileType::Symlink => {
                            trace!("is a symlink");
                            // create the symlink
                            let dest_file = PathBuf::from(stfu8::encode_u8_pretty([
                                &dest_path, 
                                &PathBuf::from(file_name),
                            ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                            trace!("dest_file={dest_file}", dest_file=dest_file.to_string_lossy());

                            if let Some(target) = &file.symlink_target {
                                info!("{path} -> {dest_file} ({target})", path=file.path.to_string_lossy(), dest_file=dest_file.to_string_lossy(), target=target.to_string_lossy());
                                if ! backup_cmd.dry_run {
                                    trace!("symlink {target} to {dest_file}", target=target.to_string_lossy(), dest_file=dest_file.to_string_lossy());
                                    if let Err(e) = std::os::unix::fs::symlink(&target, &dest_file) {
                                        if self.is_tape_full()? {
                                            tape_full = true;
                                            break;
                                        }
                                        else {
                                            Err(e)?
                                        }
                                    }
                                    // set the symlinks's mtime
                                    trace!("set symlink {dest_file} mtime", dest_file=dest_file.to_string_lossy());
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
                                }
                            }
                            else {
                                Err(eyre!("Symlink record {file:#?} has null symlink_target field!"))?;
                            }
                        },
                        FileType::File => {
                            // copy file to temp path first
                            trace!("is a file");
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
                            trace!("tmp_file_name={tmp_file_name}", tmp_file_name=tmp_file_name.to_string_lossy());

                            let tmp_file = PathBuf::from(stfu8::encode_u8_pretty([
                                &dest_path, 
                                &tmp_file_name,
                            ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                            trace!("tmp_file={tmp_file}", tmp_file=tmp_file.to_string_lossy());

                            let dest_file = PathBuf::from(stfu8::encode_u8_pretty([
                                &dest_path, 
                                &PathBuf::from(file_name),
                            ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                            trace!("dest_file={dest_file}", dest_file=dest_file.to_string_lossy());

                            info!("{path} -> {dest_file}", path=file.path.to_string_lossy(), dest_file=dest_file.to_string_lossy());
                            if ! backup_cmd.dry_run {
                                trace!("copy {file_path} to {tmp_file}", file_path=file.path.to_string_lossy(), tmp_file=tmp_file.to_string_lossy());
                                if let Err(e) = std::fs::copy(&file.path, &tmp_file) {
                                    if self.is_tape_full()? {
                                        tape_full = true;
                                        break;
                                    }
                                    else if !file.path.exists() {
                                        error!("File {} no longer exists, skipping backup!", file.path.to_string_lossy());
                                        continue;
                                    }
                                    else {
                                        Err(e)?
                                    }
                                }
                                // set the file's mtime
                                trace!("set mtime on {tmp_file}", tmp_file=tmp_file.to_string_lossy());
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
                                trace!("move {tmp_file} to {dest_file}", tmp_file=tmp_file.to_string_lossy(), dest_file=dest_file.to_string_lossy());
                                if let Err(e) = std::fs::rename(&tmp_file, dest_file) {
                                    if self.is_tape_full()? {
                                        tape_full = true;
                                        break;
                                    }
                                    else {
                                        Err(e)?
                                    }
                                }
                            }
                        },
                    };
                    // add file and backup2file records to the database
                    {
                        let tx = conn.transaction()?;
                        if let (Some(backup), Some(tape_record)) = (&backup, tape_record) {
                            let mut file_stmt = tx.prepare(r#"insert into file (tape_id,path,mtime,size,filetype,symlink_target) values (:tape_id,:path,:mtime,:size,:filetype,:symlink_target) returning *"#)?;
                            let mut b2f_stmt = tx.prepare(r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (:backup_id,:file_id,:uid,:gid,:mode,:is_deleted)"#)?;
                            // write file and backup2file db records
                            let rows = file_stmt.query(serde_rusqlite::to_params_named_with_fields(File{
                                id: 0,
                                tape_id: tape_record.id,
                                path: file.path.clone(),
                                mtime: file.mtime,
                                size: file.size,
                                filetype: file.filetype,
                                symlink_target: file.symlink_target.clone(),
                            }, &["tape_id","path","mtime","size","filetype","symlink_target"])?.to_slice().as_slice())?;
                            let mut rows_iter = serde_rusqlite::from_rows::<File>(rows);
                            if let Some(file_record) = rows_iter.next() {
                                let file_record = file_record?;
                                b2f_stmt.execute(serde_rusqlite::to_params_named(&Backup2File {
                                    backup_id: backup.id,
                                    file_id: file_record.id,
                                    uid: file.uid,
                                    gid: file.gid,
                                    mode: file.mode,
                                    is_deleted: false,
                                })?.to_slice().as_slice())?;
                            }
                        }
                        tx.commit()?;
                    }
                }
            }
            // check if we need to set old_dir's mtime
            if ! backup_cmd.dry_run {
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
            }
            // The tape has become full. 
            // we need to re-mount the filesystem to ensure we're using the most 
            // recent index that was able to be written. Then we need to
            // scan the ltfs filesystem to verify which files actually
            // were transferred successfully.
            // Any files that were not included in the last written ltfs index
            // partition will now be missing, and should be deleted from the database.
            if ! backup_cmd.dry_run && tape_full {
                info!("The tape became full, re-mounting LTFS and verifying files");
                self.unmount_tape()?;
                self.mount_tape()?;

                let mut to_delete = LinkedList::<NewFile>::new();
                let tx = conn.transaction()?;
                if let Some(backup) = &backup {
                    let sql = r#"
                    select f.id, f.path, f.mtime, f.size, b2f.uid, b2f.gid, b2f.mode, f.filetype, f.symlink_target
                    from backup2file b2f
                    join file f
                    on b2f.file_id=f.file_id
                    where b2f.backup_id=?1
                    and f.tape_id=?2
                    order by f.path nulls last
                    "#;
                    let mut stmt = tx.prepare(sql)?;
                    let backup_id = backup.id;
                    let tape = tape.as_ref().ok_or_else(|| eyre!("No tape record is set"))?;
                    let label = tape.label.as_ref().ok_or_else(|| eyre!("No label found for tape {tape}", tape=tape.index))?;
                    let tape_record = tapes.get(label).ok_or_else(|| eyre!("No tape record found for {label}"))?;
                    let rows = stmt.query(params![backup_id, tape_record.id])?;
                    let mut rows_iter = serde_rusqlite::from_rows::<NewFile>(rows);
                    while let Some(new_file) = rows_iter.next() {
                        let new_file = new_file?;
                        let new_file_path = PathBuf::from(new_file.path.parent().unwrap_or(PathBuf::from(".").as_path()));
                        let new_file_path = new_file_path.strip_prefix("/").unwrap_or_else(|_| new_file_path.as_path());

                        let path = PathBuf::from(stfu8::encode_u8_pretty([
                            &dest.as_path(),
                            &new_file_path,
                        ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                        let mut file_exists = false;
                        if path.exists() {
                            match new_file.filetype {
                                FileType::File => {
                                    if !path.is_symlink() && path.is_file() {
                                        if let Ok(md) = std::fs::symlink_metadata(&dest) {
                                            if md.size() == new_file.size && md.mtime() == new_file.mtime {
                                                file_exists = true;
                                            }
                                        }
                                    }
                                },
                                FileType::Directory => {
                                    file_exists = !path.is_symlink() && path.is_dir();
                                },
                                FileType::Symlink => {
                                    if let Some(symlink_target) = &new_file.symlink_target {
                                        if path.is_symlink() {
                                            let target = std::fs::read_link(&path)?;
                                            file_exists = target == *symlink_target;
                                        }
                                    } 
                                    else {
                                        file_exists = true;
                                    }
                                },
                            };
                        }
                        if ! file_exists {
                            to_delete.push_back(new_file);
                        }
                    }
                    // remove db records for missing files
                    for new_file in &to_delete {
                        tx.execute(r#"delete from backup2file where backup_id=?1 and file_id=?2"#, params![backup_id, new_file.id])?; 
                        tx.execute(r#"delete from file where id=?1"#, params![new_file.id])?; 
                    }
                }
                tx.commit()?;
                info!("Verification complete, removed {count} files from the database", count=to_delete.len());

                // re-enqueue the files to be copied again
                to_delete.append(&mut to_copy);
                to_copy = to_delete;
            }
        }

        // set the backup complete flag in the db
        if ! backup_cmd.dry_run {
            if let Some(backup) = &backup {
                let sql = r#"update backup set is_complete=true where backup_id=?1"#;
                debug!("Running sql update:\n{sql}\n");
                conn.execute(sql, params![backup.id])?;
            }
        }
        info!("Backup complete");
        Ok(())
    }

    fn restore_files(&self) -> Result<()> {
        debug!("Running restore_files");
        let restore_cmd = match &self.cmd {
            Some(Command::Restore(r)) => r,
            _ => Err(eyre!("Couldn't match restore subcommand!"))?,
        };
        if ! restore_cmd.restore_path.is_dir() {
            Err(eyre!("Could not find restore path: {restore_path}", restore_path=restore_cmd.restore_path.to_string_lossy()))?
        }
        debug!("Using restore_path: {}", restore_cmd.restore_path.to_string_lossy());

        // initialize the backup set's sqlite database
        self.init_db()?;
        debug!("Connecting to db {db}", db=&self.db.to_string_lossy());
        let conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<i64,Tape>::new();
        {
            let mut stmt = conn.prepare("select * from tape")?;
            let mut rows_iter = serde_rusqlite::from_rows::<Tape>(stmt.query([])?);
            while let Some(tape) = rows_iter.next() {
                let tape = tape?;
                tapes.insert(tape.id, tape);
            }
        }
        debug!("got tapes records: {tapes:#?}");

        // get the selected (or most recent if unspecified) backup record
        let mut backup = Option::<Backup>::None;
        {
            let sql = format!(r#"
            select * 
            from backup 
            where true
              {where}
              {complete}
            order by date_created desc nulls last limit 1
            "#, where=if restore_cmd.backup_id.is_some() || restore_cmd.backup_uuid.is_some() || restore_cmd.backup_date.is_some() 
                {r#"and (id=?1 or uuid=?2 or date_created <= ?3)"#} 
                else {""},
                complete=if restore_cmd.complete {"and is_complete"} else {""});
            debug!("Running query: {sql}");
            let mut stmt = conn.prepare(&sql)?;
            let rows = if restore_cmd.backup_id.is_some() || restore_cmd.backup_uuid.is_some() || restore_cmd.backup_date.is_some() {
                let backup_date = restore_cmd.backup_date.clone().map(|bd| 
                    bd.parse::<dateparser::DateTimeUtc>().ok().map(|d| 
                        d.0.timestamp())
                    ).unwrap_or(None);
                stmt.query(params![
                    restore_cmd.backup_id,
                    restore_cmd.backup_uuid,
                    &backup_date,
                ])?
            } else {
                stmt.query(params![])?
            };
            let mut rows_iter = serde_rusqlite::from_rows::<Backup>(rows);
            if let Some(backup_record) = rows_iter.next() {
                let backup_record = backup_record?;
                backup = Some(backup_record);
            }
        }

        if let Some(backup) = backup {
            debug!("Found backup record: {backup:#?}");
            let date_created = backup.date_created;
            let naive = NaiveDateTime::from_timestamp(date_created, 0);
            let datetime: DateTime<Utc> = DateTime::from_utc(naive, Utc);
            let newdate = datetime.format("%Y-%m-%d_%H.%M.%S");
            let timestamp = format!("{newdate}");
            debug!("timestamp={timestamp}");

            let source_path = [
                &self.ltfs_path, 
                &PathBuf::from("backup"),
                &PathBuf::from(
                    self.db.file_stem().unwrap_or(&OsString::from("backup"))),
                &PathBuf::from(timestamp),
                &PathBuf::from(&backup.uuid),
                &PathBuf::from("rootfs"),
            ].iter().collect::<PathBuf>();
            debug!("source_path={source_path}", source_path=source_path.to_string_lossy());

            // get the complete list of backup records from the db
            let sql = r#"
            with recursive parent_backup(id, date_created, uuid, parent_backup_id, depth) as (
              select id, date_created, uuid, parent_backup_id, 0 as depth
              from backup
              where id = ?1
              union all
              select b.id, b.date_created, b.uuid, b.parent_backup_id, pb.depth+1 as depth
              from backup b
              join parent_backup pb 
                on pb.parent_backup_id = b.id) 
            select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype, f.symlink_target
            from (parent_backup pb
              join backup2file b2f
                on b2f.backup_id = pb.id
              join file f
                on b2f.file_id=f.id)
            join (parent_backup pb2
              join backup2file b2f2
                on b2f2.backup_id=pb2.id
              join file f2
                on b2f2.file_id=f2.id)
            on f.path=f2.path
              and f.mtime=f2.mtime
              and f.size=f2.size
              and f.filetype=f2.filetype
              and pb.depth <= pb2.depth
            group by 1,2,3,4,5,6,7,8,9,10,11,12
            having count(*)=1
            order by f.tape_id nulls last, f.path nulls last
            "#;
            debug!("Running sql query:\n{sql}\n");
            let mut stmt = conn.prepare(sql)?;
            let mut old_tape_id = Option::<i64>::None;
            let mut old_dir = Option::<(PathBuf, i64)>::None;
            let rows = stmt.query([&backup.id])?;
            let mut rows_iter = serde_rusqlite::from_rows::<FullBackup>(rows);
            while let Some(full_backup) = rows_iter.next() {
                let full_backup = full_backup?;
                if ! restore_cmd.patterns.is_empty() {
                    let mut found_match = false;
                    for pattern in &restore_cmd.patterns {
                        if pattern.matches_path(&full_backup.path) {
                            found_match = true;
                            break;
                        }
                        let recursive_pattern = Pattern::new(&regex_replace!(r#"/+(\*\*+)?$"#, pattern.as_str(), |_,_| "/**"))?;
                        if recursive_pattern.matches_path(&full_backup.path) {
                            found_match = true;
                            break;
                        }
                    }
                    if ! found_match {
                        continue;
                    }
                }
                // check include filters
                if !restore_cmd.include_regex.is_empty() {
                    let mut to_include = false;
                    for i in &restore_cmd.include_regex {
                        if i.is_match(full_backup.path.as_os_str().as_bytes()) {
                            to_include = true;
                            break;
                        }
                    }
                    if !to_include {
                        continue;
                    }
                }
                if !restore_cmd.include.is_empty() {
                    let mut to_include = false;
                    for i in &restore_cmd.include {
                        if i.matches_path(&full_backup.path) {
                            to_include = true;
                            break;
                        }
                    }
                    if !to_include {
                        continue;
                    }
                }
                // check exclude filters
                if !restore_cmd.exclude_regex.is_empty() {
                    for e in &restore_cmd.exclude_regex {
                        if e.is_match(full_backup.path.as_os_str().as_bytes()) {
                            continue;
                        }
                    }
                }
                if !restore_cmd.exclude.is_empty() {
                    for e in &restore_cmd.exclude {
                        if e.matches_path(&full_backup.path) {
                            continue;
                        }
                    }
                }

                let full_backup_path = PathBuf::from(full_backup.path.parent().unwrap_or(PathBuf::from(".").as_path()));
                let full_backup_path = full_backup_path.strip_prefix("/").unwrap_or_else(|_| full_backup_path.as_path());
                let source = PathBuf::from(OsString::from_vec(stfu8::decode_u8(&[
                    &source_path.as_path(),
                    &full_backup_path,
                ].iter().collect::<PathBuf>().to_string_lossy().to_string())?));
                let dest = [
                    &restore_cmd.restore_path.as_path(),
                    &full_backup_path,
                ].iter().collect::<PathBuf>();

                if let Some(parent) = dest.parent() {
                    // set the mtime of the previous directory if it's done
                    // being written
                    if let Some((od, mtime)) = &old_dir {
                        if od != parent {
                            let filetime = FileTime::from_unix_time(*mtime, 0);
                            filetime::set_file_times(od, filetime.clone(), filetime.clone())?;
                            old_dir = None;
                        }
                    }
                    std::fs::create_dir_all(parent)?;
                }
                // transfer the file
                match full_backup.filetype {
                    FileType::Directory => {
                        info!("{source}/ -> {dest}/", source=source.to_string_lossy(), dest=dest.to_string_lossy());
                        let mut mtime = full_backup.mtime;
                        if ! restore_cmd.dry_run {
                            nix::unistd::mkdir(&dest, Mode::from_bits_truncate(full_backup.mode))?;
                            nix::unistd::chown(&dest, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                            if let Ok(md) = std::fs::symlink_metadata(&dest) {
                                let current_mtime = FileTime::from_last_modification_time(&md);
                                mtime = std::cmp::max(mtime, current_mtime.unix_seconds());
                            }
                        }
                        old_dir = Some((dest, mtime));
                    },
                    FileType::Symlink => {
                        if dest.exists() && restore_cmd.noclobber {
                            continue;
                        }
                        if dest.is_dir() {
                            Err(eyre!("Destination {dest} is a directory instead of a symlink!", dest=dest.to_string_lossy()))?;
                        }
                        let target = full_backup.symlink_target.ok_or_else(|| eyre!("Symlink target is null!"))?;
                        info!("{source} -> {dest} ({target})", source=source.to_string_lossy(), dest=dest.to_string_lossy(), target=target.to_string_lossy());
                        if ! restore_cmd.dry_run {
                            std::os::unix::fs::symlink(&target, &dest)?;
                            nix::unistd::chown(&dest, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                            let filetime = FileTime::from_unix_time(full_backup.mtime, 0);
                            filetime::set_symlink_file_times(&dest, filetime.clone(), filetime.clone())?;
                        }
                    },
                    FileType::File => {
                        if dest.exists() {
                            if restore_cmd.noclobber {
                                continue;
                            } 
                            if dest.is_dir() {
                                Err(eyre!("Destination {dest} is a directory instead of a file!", dest=dest.to_string_lossy()))?;
                            }

                            // if mtime and size is the same, skip copying the file
                            let attr = std::fs::symlink_metadata(&dest)?;
                            if dest.is_file() && attr.mtime() == full_backup.mtime && attr.size() == full_backup.size {
                                if ! restore_cmd.dry_run {
                                    // set uid:gid
                                    nix::unistd::chown(&dest, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                                    // set mode
                                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(full_backup.mode))?;
                                }
                                continue;
                            }
                            if ! restore_cmd.dry_run {
                                // remove existing destination file to break any hard links
                                std::fs::remove_file(&dest)?;
                            }
                        }
                        if let Some(dest_path) = dest.parent() {
                            if let Some(file_name) = dest.file_name() {
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
                                let tmp_file = [
                                    &PathBuf::from(dest_path), 
                                    &tmp_file_name,
                                ].iter().collect::<PathBuf>();

                                info!("{source} -> {dest}", source=source.to_string_lossy(), dest=dest.to_string_lossy());

                                if ! restore_cmd.dry_run {
                                    // make sure correct tape is loaded and mounted
                                    if old_tape_id != Some(full_backup.tape_id) {
                                        // get loader status
                                        if let Some(tape) = tapes.get(&full_backup.tape_id) {
                                            let ls = self.loader_status()?;
                                            if let Some(se) = ls.iter().filter(|se| if let Some(label) = &se.label {*label==tape.label} else {false}).next() {
                                                debug!("Loading tape {label}", label=&tape.label);
                                                self.unmount_tape()?;
                                                self.load_tape(se.index)?;
                                                self.mount_tape()?;
                                            }
                                            else {
                                                Err(eyre!("Could not find tape with label {label} in loader!", label=tape.label))?
                                            }
                                        }
                                        else {
                                            Err(eyre!("Could not find database record for tape with id {label}!", label=full_backup.tape_id))?
                                        }
                                        old_tape_id = Some(full_backup.tape_id);
                                    }
                                    // copy the file
                                    std::fs::copy(&source, &tmp_file)?;
                                    // set uid:gid
                                    nix::unistd::chown(&tmp_file, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                                    // set mode
                                    std::fs::set_permissions(&tmp_file, std::fs::Permissions::from_mode(full_backup.mode))?;
                                    // set the file's mtime
                                    let mtime = FileTime::from_unix_time(full_backup.mtime, 0);
                                    filetime::set_file_times(&tmp_file, mtime.clone(), mtime.clone())?;
                                    // then move file into place
                                    std::fs::rename(&tmp_file, dest)?;
                                }
                            }
                            else {
                                Err(eyre!("Could not parse dest file_name!"))?;
                            }
                        }
                        else {
                            Err(eyre!("Could not parse dest parent!"))?;
                        }
                    },
                };
            }
            // set the mtime for the previous directory
            if let Some((old_dir, mtime)) = &old_dir {
                let filetime = FileTime::from_unix_time(*mtime, 0);
                if ! restore_cmd.dry_run {
                    filetime::set_file_times(old_dir, filetime.clone(), filetime.clone())?;
                }
            }
        }
        else {
            Err(eyre!("No backup found matching backup_id {backup_id:#?}", backup_id=restore_cmd.backup_id))?
        }
        info!("Restore complete");
        Ok(())
    }

    fn list_backups(&self) -> Result<()> {
        debug!("Running list_backups");
        let list_backups_cmd = match &self.cmd {
            Some(Command::ListBackups(l)) => l,
            _ => Err(eyre!("Couldn't match subcommand!"))?,
        };

        let pager = std::env::var("PAGER").ok().
            map(|p| PathBuf::from(p)).
            or_else(|| which("pspg").ok()).
            or_else(|| which("less").ok());
        debug!("Using pager: {pager:#?}");
        let mut process = if let Some(pager) = pager {
            Some(std::process::Command::new(pager).
                stdin(std::process::Stdio::piped()).
                spawn()?)
        } else { None };
        let writer: Box<dyn std::io::Write> = if let Some(process) = &mut process {
            if let Some(stdin) = &mut process.stdin {
                Box::new(stdin)
            }
            else {
                Err(eyre!("Could not open process stdin!"))?
            }
        } else {
            Box::new(std::io::stdout())
        };
        let mut csv_writer = csv::Writer::from_writer(writer);

        self.init_db()?;
        debug!("Connecting to database {db}", db=self.db.to_string_lossy());
        let conn = Connection::open(&self.db)?;
        {
            let sql = format!(r#"
            select * 
            from backup 
            where true
              {where}
              {complete}
            order by date_created
            "#, where=if list_backups_cmd.backup_id.is_some() || list_backups_cmd.backup_uuid.is_some() || list_backups_cmd.backup_date.is_some() 
                {r#"where (id=?1 or uuid=?2 or date_created <= ?3)"#} 
                else {""},
                complete=if list_backups_cmd.complete {"and is_complete"} else {""});
            debug!("Running sql query:\n{sql}\n");
            let mut stmt = conn.prepare(&sql)?;
            let cols = stmt.column_names().iter().map(|c| c.to_string()).collect::<Vec<_>>();
            let rows = if list_backups_cmd.backup_id.is_some() || list_backups_cmd.backup_uuid.is_some() || list_backups_cmd.backup_date.is_some() {
                let backup_date = list_backups_cmd.backup_date.clone().map(|bd| 
                    bd.parse::<dateparser::DateTimeUtc>().ok().map(|d| 
                        d.0.timestamp())
                    ).unwrap_or(None);
                stmt.query(params![
                    list_backups_cmd.backup_id,
                    list_backups_cmd.backup_uuid,
                    &backup_date,
                ])?
            } else {
                stmt.query(params![])?
            };
            csv_writer.write_record(cols.as_slice())?;

            let mut rows_iter = serde_rusqlite::from_rows::<Backup>(rows);
            while let Some(backup) = rows_iter.next() {
                let backup = backup?;
                csv_writer.serialize(backup)?;
            }
        }
        Ok(())
    }

    fn list_files(&self) -> Result<()> {
        debug!("Running list_files");
        let list_files_cmd = match &self.cmd {
            Some(Command::ListFiles(l)) => l,
            _ => Err(eyre!("Couldn't match subcommand!"))?,
        };

        let pager = std::env::var("PAGER").ok().
            map(|p| PathBuf::from(p)).
            or_else(|| which("pspg").ok()).
            or_else(|| which("less").ok());
        debug!("Using pager: {pager:#?}");
        let mut process = if let Some(pager) = pager {
            Some(std::process::Command::new(pager).
                stdin(std::process::Stdio::piped()).
                spawn()?)
        } else { None };
        let writer: Box<dyn std::io::Write> = if let Some(process) = &mut process {
            if let Some(stdin) = &mut process.stdin {
                Box::new(stdin)
            }
            else {
                Err(eyre!("Could not open process stdin!"))?
            }
        } else {
            Box::new(std::io::stdout())
        };
        let mut csv_writer = csv::Writer::from_writer(writer);

        self.init_db()?;
        debug!("Connecting to database {db}", db=self.db.to_string_lossy());
        let conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<i64,Tape>::new();
        {
            let mut stmt = conn.prepare("select * from tape")?;
            let mut rows_iter = serde_rusqlite::from_rows::<Tape>(stmt.query([])?);
            while let Some(tape) = rows_iter.next() {
                let tape = tape?;
                tapes.insert(tape.id, tape);
            }
        }
        debug!("got tapes records: {tapes:#?}");

        let mut backup = Option::<Backup>::None;
        {
            let sql = format!(r#"
            select *
            from backup b
            {where}
            order by date_created desc
            limit 1
            "#, where=if list_files_cmd.backup_id.is_some() || list_files_cmd.backup_uuid.is_some() || list_files_cmd.backup_date.is_some() 
                {r#"where (b.id=?1 or b.uuid=?2 or b.date_created <= ?3)"#} 
                else {""});
            let mut stmt = conn.prepare(&sql)?;
            let rows = if list_files_cmd.backup_id.is_some() || list_files_cmd.backup_uuid.is_some() || list_files_cmd.backup_date.is_some() {
                let backup_date = list_files_cmd.backup_date.clone().map(|bd| 
                    bd.parse::<dateparser::DateTimeUtc>().ok().map(|d| 
                        d.0.timestamp())
                    ).unwrap_or(None);
                stmt.query(params![
                    list_files_cmd.backup_id,
                    list_files_cmd.backup_uuid,
                    &backup_date,
                ])?
            } else {
                stmt.query(params![])?
            };
            let mut rows_iter = serde_rusqlite::from_rows::<Backup>(rows);
            if let Some(backup_record) = rows_iter.next() {
                let backup_record = backup_record?;
                backup = Some(backup_record);
            }
        }

        let cache = users::UsersCache::new();
        if let Some(backup) = backup {
            debug!("Using backup record: {backup:#?}");
            let sql = r#"
            with recursive parent_backup(id, date_created, uuid, parent_backup_id, depth) as (
              select id, date_created, uuid, parent_backup_id, 0 as depth
              from backup
              where id = ?1
              union all
              select b.id, b.date_created, b.uuid, b.parent_backup_id, pb.depth+1 as depth
              from backup b
              join parent_backup pb 
                on pb.parent_backup_id = b.id) 
            select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype, f.symlink_target
            from (parent_backup pb
              join backup2file b2f
                on b2f.backup_id = pb.id
              join file f
                on b2f.file_id=f.id)
            join (parent_backup pb2
              join backup2file b2f2
                on b2f2.backup_id=pb2.id
              join file f2
                on b2f2.file_id=f2.id)
            on f.path=f2.path
              and f.mtime=f2.mtime
              and f.size=f2.size
              and f.filetype=f2.filetype
              and pb.depth <= pb2.depth
            group by 1,2,3,4,5,6,7,8,9,10,11,12
            having count(*)=1
            order by f.path nulls last
            "#;
            debug!("Running sql query:\n{sql}\n");
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query([&backup.id])?;
            let mut rows_iter = serde_rusqlite::from_rows::<FullBackup>(rows);
            while let Some(full_backup) = rows_iter.next() {
                let full_backup = full_backup?;
                if ! list_files_cmd.patterns.is_empty() {
                    let mut found_match = false;
                    for pattern in &list_files_cmd.patterns {
                        if pattern.matches_path(&full_backup.path) {
                            found_match = true;
                            break;
                        }
                        let recursive_pattern = Pattern::new(&regex_replace!(r#"/+(\*\*+)?$"#, pattern.as_str(), |_,_| "/**"))?;
                        if recursive_pattern.matches_path(&full_backup.path) {
                            found_match = true;
                            break;
                        }
                    }
                    if ! found_match {
                        continue;
                    }
                }
                // check include filters
                if !list_files_cmd.include_regex.is_empty() {
                    let mut to_include = false;
                    for i in &list_files_cmd.include_regex {
                        if i.is_match(full_backup.path.as_os_str().as_bytes()) {
                            to_include = true;
                            break;
                        }
                    }
                    if !to_include {
                        continue;
                    }
                }
                if !list_files_cmd.include.is_empty() {
                    let mut to_include = false;
                    for i in &list_files_cmd.include {
                        if i.matches_path(&full_backup.path) {
                            to_include = true;
                            break;
                        }
                    }
                    if !to_include {
                        continue;
                    }
                }
                // check exclude filters
                if !list_files_cmd.exclude_regex.is_empty() {
                    for e in &list_files_cmd.exclude_regex {
                        if e.is_match(full_backup.path.as_os_str().as_bytes()) {
                            continue;
                        }
                    }
                }
                if !list_files_cmd.exclude.is_empty() {
                    for e in &list_files_cmd.exclude {
                        if e.matches_path(&full_backup.path) {
                            continue;
                        }
                    }
                }
                let tape = tapes.get(&full_backup.tape_id).
                    ok_or_else(|| eyre!("Could not find tape label for id {id}", id=full_backup.tape_id))?;

                let naive = NaiveDateTime::from_timestamp(full_backup.mtime, 0);
                let datetime: DateTime<Utc> = DateTime::from_utc(naive, Utc);
                let newdate = datetime.format("%Y-%m-%d_%H.%M.%S");
                let timestamp = format!("{newdate}");
                let path = stfu8::encode_u8_pretty(full_backup.path.as_os_str().as_bytes());
                let mode = file_mode::Mode::from(full_backup.mode);
                let is_executable = mode.user_protection(file_mode::User::Owner).is_execute_set() || 
                    mode.user_protection(file_mode::User::Group).is_execute_set() ||
                    mode.user_protection(file_mode::User::Other).is_execute_set();
                let path = format!("{path}{decoration}", 
                    decoration=match full_backup.filetype {
                        FileType::Directory => "/",
                        FileType::Symlink => " -> ",
                        FileType::File => if is_executable {"*"} else {""},
                    });
                let symlink_target = full_backup.symlink_target.map(|p| stfu8::encode_u8_pretty(p.as_os_str().as_bytes()));

                csv_writer.write_record([
                    &file_mode::Mode::from(full_backup.mode).to_string(),
                    &cache.get_user_by_uid(full_backup.uid).
                        map(|u| u.name().to_string_lossy().into_owned()).
                        unwrap_or_else(|| full_backup.uid.to_string()),
                    &cache.get_group_by_gid(full_backup.gid).
                        map(|u| u.name().to_string_lossy().into_owned()).
                        unwrap_or_else(|| full_backup.gid.to_string()),
                    &full_backup.size.to_string(),
                    &timestamp,
                    &tape.label,
                    &path,
                    &symlink_target.unwrap_or_else(|| "".to_string()),
                ])?;
            }
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    std::env::set_var("RUST_BACKTRACE", "full");
    color_eyre::install()?;

    let mut options = Options::from_args();

    let logspec = if options.trace {"TRACE"} else if options.verbose {"DEBUG"} else {"INFO"};
    Logger::try_with_env_or_str(logspec)?.format(flexi_logger::colored_detailed_format).start()?;
    debug!("Parsed options: {options:#?}");

    options.mtx = match which(&options.mtx) {
        Ok(path) => path,
        Err(e) => Err(eyre!("Could not find mtx program: {mtx}: {e}", mtx=options.mtx.display()))?,
    };
    options.mt = match which(&options.mt) {
        Ok(path) => path,
        Err(e) => Err(eyre!("Could not find mt program: {mt}: {e}", mt=options.mt.display()))?,
    };
    options.ltfs = match which(&options.ltfs) {
        Ok(path) => path,
        Err(e) => Err(eyre!("Could not find ltfs program: {ltfs}: {e}", ltfs=options.ltfs.display()))?,
    };
    options.mkltfs = match which(&options.mkltfs) {
        Ok(path) => path,
        Err(e) => Err(eyre!("Could not find ltfs program: {mkltfs}: {e}", mkltfs=options.mkltfs.display()))?,
    };
    options.umount = match which(&options.umount) {
        Ok(path) => path,
        Err(e) => Err(eyre!("Could not find umount program: {umount}: {e}", umount=options.umount.display()))?,
    };

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

    match options.cmd {
        Some(Command::Backup(_)) => {
            options.backup_files()?;
        },
        Some(Command::Restore(_)) => {
            options.restore_files()?;
        },
        Some(Command::ListBackups(_)) => {
            options.list_backups()?;
        },
        Some(Command::ListFiles(_)) => {
            options.list_files()?;
        },
        _ => {
            eprintln!("Must specify one of: backup, restore, listbackups, listfiles");
            std::process::exit(1);
        }
    };

    Ok(())
}
