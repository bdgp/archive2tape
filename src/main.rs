#![cfg_attr(feature = "cargo-clippy", allow(cyclomatic_complexity, trivial_regex))]
use std::os::unix::prelude::MetadataExt;
use std::path::{PathBuf};
use std::io::{BufRead, BufReader};
use std::collections::{HashSet, HashMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, NaiveDateTime, Utc};
use num_enum::{TryFromPrimitive, IntoPrimitive};
use regex::Regex;
use glob::Pattern;

use serde::{Serializer, Deserializer, Deserialize, de};
use clap::{Parser, Subcommand};
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
use pager::Pager;
use flexi_logger::{FileSpec, Criterion, Naming, Cleanup, Duplicate};

// /dev/sg0 lto5 drive
// /dev/sg1 lto5 loader
// /dev/sg2 lto8 drive
// /dev/sg4 lto8 loader

// TODO: copy checksums, tape tags

#[derive(Parser, Debug)]
#[clap( about = "Perform a backup to tape")]
struct BackupCmd {
    #[clap(long, help = "Regex patterns to include files", name="INCLUDE_PATTERN")]
    include: Vec<Pattern>,
    #[clap(long, help = "Regex patterns to exclude files", name="EXCLUDE_PATTERN")]
    exclude: Vec<Pattern>,
    #[clap(long, help = "Regex patterns to include files", name="INCLUDE_REGEX")]
    include_regex: Vec<regex::bytes::Regex>,
    #[clap(long, help = "Regex patterns to exclude files", name="EXCLUDE_REGEX")]
    exclude_regex: Vec<regex::bytes::Regex>,
    #[clap(long, help = "Do not actually backup any files, just show what files would be backed up")]
    dry_run: bool,
    #[clap(long, help = "Perform a test of the tape full handling routine at the end of this backup.")]
    tape_full_test: bool,
    #[clap(long, help = "Force a full backup instead of an incremental backup")]
    full: bool,
    #[clap(help = "Glob patterns to match files in filesystem", name="GLOB_PATTERN")]
    patterns: Vec<Pattern>,
}

#[derive(Parser, Debug)]
#[clap( about = "Perform a restore from a previous backup")]
struct RestoreCmd {
    #[clap(long, help = "Restore path", name="RESTORE_PATH", default_value=".")]
    restore_path: PathBuf,
    #[clap(long, help = "Do not ovewrite existing files during a restore")]
    noclobber: bool,
    #[clap(long, help = "Backup ID to use for restore (default is latest backup)", name="BACKUP_ID")]
    backup_id: Option<i64>,
    #[clap(long, help = "Backup UUID to use for restore (default is latest backup)", name="BACKUP_UUID")]
    backup_uuid: Option<String>,
    #[clap(long, help = "Backup date to use for restore (default is latest backup)", name="BACKUP_DATETIME")]
    backup_date: Option<String>,
    #[clap(long, help = "Regex patterns to include files", name="INCLUDE_PATTERN")]
    include: Vec<Pattern>,
    #[clap(long, help = "Regex patterns to exclude files", name="EXCLUDE_PATTERN")]
    exclude: Vec<Pattern>,
    #[clap(long, help = "Regex patterns to include files", name="INCLUDE_REGEX")]
    include_regex: Vec<regex::bytes::Regex>,
    #[clap(long, help = "Regex patterns to exclude files", name="EXCLUDE_REGEX")]
    exclude_regex: Vec<regex::bytes::Regex>,
    #[clap(long, help = "Do not actually restore any files, just show what files would be restored")]
    dry_run: bool,
    #[clap(long, help = "Remove database records if the file does not exist on tape")]
    verify_db: bool,
    #[clap(long, help = "List tapes required for this restore command")]
    list_tapes: bool,
    #[clap(long, help = "Only restore from complete backups")]
    complete: bool,
    #[clap(help = "Glob patterns to filter restore files", name="PATTERN")]
    patterns: Vec<Pattern>,
}

#[derive(Parser, Debug)]
#[clap( about = "List all backups in the sqlite database")]
struct ListBackupsCmd { 
    #[clap(long = "backup-id", help = "Backup ID to search", name="BACKUP_ID")]
    backup_id: Option<i64>,
    #[clap(long = "backup-uuid", help = "Backup UUID to search", name="BACKUP_UUID")]
    backup_uuid: Option<String>,
    #[clap(long = "backup-date", help = "Backup date to search", name="BACKUP_DATETIME")]
    backup_date: Option<String>,
    #[clap(long = "complete", help = "Only show complete backups")]
    complete: bool,
}

#[derive(Parser, Debug)]
#[clap( about = "List files in a given backup")]
struct ListFilesCmd {
    #[clap(long, help = "Backup ID to search", name="BACKUP_ID")]
    backup_id: Option<i64>,
    #[clap(long, help = "Backup UUID to search", name="BACKUP_UUID")]
    backup_uuid: Option<String>,
    #[clap(long, help = "Backup date to search", name="BACKUP_DATETIME")]
    backup_date: Option<String>,
    #[clap(long, help = "Regex patterns to include files", name="INCLUDE_PATTERN")]
    include: Vec<Pattern>,
    #[clap(long, help = "Regex patterns to exclude files", name="EXCLUDE_PATTERN")]
    exclude: Vec<Pattern>,
    #[clap(long, help = "Regex patterns to include files", name="INCLUDE_REGEX")]
    include_regex: Vec<regex::bytes::Regex>,
    #[clap(long, help = "Regex patterns to exclude files", name="EXCLUDE_REGEX")]
    exclude_regex: Vec<regex::bytes::Regex>,
    #[clap(help = "Glob patterns to filter search results", name="GLOB_PATTERN")]
    patterns: Vec<Pattern>,
}

#[derive(Parser, Debug)]
#[clap( about = "Move a new tape into the drive")]
struct LoadTapeCmd {
    #[clap(help = "Load a tape with a given slot number", name="SLOT_NUMBER")]
    slot: usize,
}

#[derive(Parser, Debug)]
#[clap( about = "Format a tape using LTFS")]
struct FormatLTFSCmd {
    #[clap(long, help = "Format tape even if it's alrady formatted?")]
    force: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    Backup(BackupCmd),
    Restore(RestoreCmd),
    ListBackups(ListBackupsCmd),
    ListFiles(ListFilesCmd),
    #[clap( about = "List the status of all tapes in the db and loader")]
    ListTapes,
    FormatLTFS(FormatLTFSCmd),

    #[clap( about = "Test if the LTFS filesystem is mounted")]
    IsMounted,
    #[clap( about = "Test if the current tape is full")]
    IsTapeFull,
    #[clap( about = "List all the tapes in the loader")]
    LoaderStatus,
    LoadTape(LoadTapeCmd),
    #[clap( about = "Mount a tape using LTFS")]
    MountTape,
    #[clap( about = "Find and load a tape that is ready for writing")]
    ReadyStorage,
    #[clap( about = "Return the tape records from the sqlite database")]
    TapeStatus,
    #[clap( about = "Unmount the LTFS volume")]
    UnmountTape,
    #[clap( about = "Wait until the drive is ready")]
    WaitForDrive,
    #[clap( about = "Wait until the ltfs process has terminated")]
    WaitForLTFSProcessToTerminate,
}

#[derive(Parser, Debug)]
#[clap(name = "backup2tape", about = "Incremental tape archiving tool", global_settings = &[clap::AppSettings::ColoredHelp, clap::AppSettings::UnifiedHelpMessage])]
struct Options {
    #[clap(long, help = "Path to tape drive device", name="DRIVE_DEVICE", default_value="/dev/st1")]
    drive: PathBuf,
    #[clap(long, help = "Path to tape drive device to use for ltfs", name="LTFS_DRIVE_DEVICE", default_value="/dev/sg2")]
    ltfs_drive: PathBuf,
    #[clap(long, help = "Path to loader device", name="LOADER_DEVICE", default_value="/dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0")]
    loader: PathBuf,
    #[clap(long, help = "Path to mtx command (from mtx package)", name="MTX_PATH", default_value="mtx")]
    mtx: PathBuf,
    #[clap(long, help = "Path to mt command (from cpio package)", name="MT_PATH", default_value="mt")]
    mt: PathBuf,
    #[clap(long, help = "Path to ltfs command", name="LTFS_PATH", default_value="ltfs")]
    ltfs: PathBuf,
    #[clap(long, help = "Use a custom shell command to mount the ltfs filesystem", name="MOUNT_COMMAND")]
    mount_command: Option<String>,
    #[clap(long, help = "Use a custom shell command to unmount the ltfs filesystem", name="UNMOUNT_COMMAND")]
    unmount_command: Option<String>,
    #[clap(long, help = "Path to mkltfs command", name="MKLTFS_PATH", default_value="mkltfs")]
    mkltfs: PathBuf,
    #[clap(long, help = "Path to fusermount command", name="FUSERMOUNT_PATH", default_value="fusermount")]
    fusermount: PathBuf,
    #[clap(long, help = "Path to SQLite backup metadata database", name="DB_PATH", default_value="./archive2tape.db")]
    db: PathBuf,
    #[clap(long, help = "Write to log file at specified path", name="LOG_PATH")]
    log: Option<PathBuf>,
    #[clap(long, help = "Path to SQLite tapes metadata database", name="TAPES_DB_PATH")]
    tapes_db: Option<PathBuf>,
    #[clap(long, help = "Create the metadata database if it doesn't exist")]
    create_db: bool,
    #[clap(long, help = "Mount path for LTFS filesystem", name="LTFS_MOUNT_PATH", default_value="/media/lto8")]
    ltfs_path: PathBuf,
    #[clap(long, help = "List of tapes to use for backup and restore. If none specified, use any tapes", name="TAPE_LABEL")]
    tapes: Vec<String>,
    #[clap(long, help = "Regex patterns to ignore tapes with matching label", name="IGNORE_TAPE_REGEX", default_value="^CLN")]
    ignore_tape: Regex,
    #[clap(long, help = "Run in debug logging mode")]
    verbose: bool,
    #[clap(long, help = "Run in trace logging mode")]
    trace: bool,
    #[clap(subcommand)]
    cmd: Option<Command>,
}


#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
enum StorageType {
    Drive,
    Element,
    ImportExport,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
enum StorageStatus {
    Empty,
    Full,
}

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Serialize, Deserialize)]
struct TapeStatus {
    label: String,
    is_full: Option<bool>,
    date_added: Option<String>,
    storage_type: Option<StorageType>,
    index: Option<usize>,
    storage_element_index: Option<usize>,
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
struct ListTape {
    label: String,
    is_loaded: bool,
    is_required: bool,
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
            else {
                debug!("Could not parse line: {line}");
            }
        }
        debug!("elements.len: {}", elements.len());
        debug!("elements: {elements:#?}");
        Ok(elements)
    }

    fn init_pager(&self) -> Result<()> {
        let pager_cmd = if let Ok(pspg) = which("pspg") {
            Some(format!("{} --csv --csv-header on", pspg.to_string_lossy()))
        } 
        else if let Ok(less) = which("less") {
            Some(less.to_string_lossy().to_string())
        }
        else {None};
        debug!("Using pager: {pager_cmd:#?}");
        let mut pager = if let Some(pager_cmd) = &pager_cmd {
            Pager::with_pager(pager_cmd)
        }
        else {
            Pager::new()
        };
        pager.setup();
        Ok(())
    }

    fn loader_status_view(&self) -> Result<()> {
        let ls = self.loader_status()?;
        self.init_pager()?;

        let mut csv_writer = csv::Writer::from_writer(std::io::stdout());
        for se in &ls {
            csv_writer.serialize(se)?;
        }
        Ok(())
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
        Ok(false)
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
            let rows = stmt.query(params![])?;
            let mut rows_iter = serde_rusqlite::from_rows::<Tape>(rows);
            while let Some(tape) = rows_iter.next() {
                let tape = tape?;
                tapes.push(tape);
            }
        }
        debug!("Got tapes: {tapes:#?}");
        Ok(tapes)
    }

    fn insert_tape_record(&self, label: &str) -> Result<Tape> {
        debug!("Running insert_tape_record");
        if let Some(tapes_db) = &self.tapes_db {
            debug!("Loading tapes db {}", tapes_db.display());
            let tapes_conn = Connection::open(tapes_db)?;
            debug!("Inserting tape record {label} into tapes db");
            let mut stmt = tapes_conn.prepare(r#"insert into tape (label) values (:label) on conflict(label) do nothing returning *"#)?;
            let tape = Tape{
                id: 0,
                label: label.to_string(),
                is_full: false,
                date_added: "".to_string(),
            };
            let rows = stmt.query(serde_rusqlite::to_params_named_with_fields(&tape, &["label"])?.to_slice().as_slice())?;
            let mut rows_iter = serde_rusqlite::from_rows::<Tape>(rows);
            if let Some(tape_record) = rows_iter.next() {
                let tape_record = tape_record?;
                trace!("query returned tape record: {tape_record:#?}");

                let conn = Connection::open(&self.db)?;
                debug!("Inserting tape record {tape_record:#?} into metadata db");
                let mut stmt = conn.prepare(r#"insert into tape (id,label,is_full,date_added) values (:id,:label,:is_full,:date_added) on conflict(label) do nothing"#)?;
                stmt.execute(serde_rusqlite::to_params_named_with_fields(&tape_record, &["id","label","is_full","date_added"])?.to_slice().as_slice())?;
                return Ok(tape_record)
            }
        }
        else {
            Err(eyre!("No tapes db given!"))?
        }
        Err(eyre!("Could not reeturn tape record!"))
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
        'WAIT_FOR_DRIVE:
        for _ in 0..300 {
            debug!("Running mt status command");
            let mt_cmd = cmd!(&self.mt,"-f", &self.drive, "status");
            let reader = mt_cmd.reader()?;
            let reader = BufReader::new(reader);

            for line in reader.lines() {
                let line = line?;
                if regex_is_match!(r#"ONLINE"#, &line) {
                    debug!("Drive is ready");
                    drive_is_ready = true;
                    break 'WAIT_FOR_DRIVE;
                }
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
        //for _ in 0..3600 {
        loop {
            trace!("Refreshing processes");
            system.refresh_processes();
            let mut found_ltfs = false;
            for (_pid, process) in system.processes() {
                trace!("Checking process: {process:#?} with name \"{name}\", exe \"{exe}\", cmd \"{cmd:#?}\"",
                    name=process.name(),
                    exe=process.exe().to_string_lossy(),
                    cmd=process.cmd());
                if process.name() == "ltfs" || 
                    regex_is_match!(r#"(^|/)ltfs$"#, &process.exe().to_string_lossy()) || (!process.cmd().is_empty() && regex_is_match!(r#"(^|/)ltfs$"#, &process.cmd()[0]))
                {
                    trace!("Found matching process");
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
        //Err(eyre!("ltfs process took too long to terminate"))?
    }

    fn unmount_tape(&self) -> Result<()> {
        info!("Unmounting current tape");
        if self.is_mounted()? {
            if let Some(unmount_command) = &self.unmount_command {
                cmd!("/bin/sh","-c",unmount_command).run()?;
            }
            else {
                debug!("Running fusermount command");
                cmd!(&self.fusermount, "-u", &self.ltfs_path).run()?;
            }
            self.wait_for_ltfs_process_to_terminate()?;
        }
        Ok(())
    }

    fn load_tape(&self, slot: usize) -> Result<()> {
        debug!("Running load_tape");
        let ls = self.loader_status()?;
        let se = ls.iter().
            filter(|se| se.index==slot).
            next().ok_or(eyre!("Could not find storage element for slot {slot}!"))?;
        let drive_slot = ls.iter().
            filter(|se| se.storage_type==StorageType::Drive).
            next().ok_or(eyre!("Could not find drive slot"))?;
        debug!("Found drive slot: {drive_slot:#?}");

        if drive_slot.status == StorageStatus::Full {
            debug!("Drive slot is full");
            let empty_slot = drive_slot.storage_element_index.
                and_then(|i| 
                    ls.iter().filter(|se| 
                        se.index==i && 
                        se.storage_type==StorageType::Element && 
                        se.status == StorageStatus::Empty).next()).
                or_else(||
                    ls.iter().filter(|se| 
                        se.storage_type==StorageType::Element && 
                        se.status==StorageStatus::Empty).next()).
                ok_or(eyre!("Could not find an empty slot"))?;
            debug!("Found empty slot: {empty_slot:#?}");
            
            self.unmount_tape()?;
            debug!("Running unload command");
            info!("Moving tape {:#?} from drive to empty slot {}", drive_slot.label, empty_slot.index);
            cmd!(&self.mtx, "-f", &self.loader, "unload", empty_slot.index.to_string(), drive_slot.index.to_string()).run()?;
        }
        debug!("Running load commmand");
        info!("Moving tape {:#?} from slot {} to drive", se.label, slot);
        cmd!(&self.mtx, "-f", &self.loader, "load", slot.to_string(), drive_slot.index.to_string()).run()?;
        self.wait_for_drive()?;
        Ok(())
    }

    fn mount_tape(&self) -> Result<()> {
        info!("Mounting current tape");
        if ! self.is_mounted()? {
            debug!("Running ltfs commmand");

            if let Some(mount_command) = &self.mount_command {
                cmd!("/bin/sh","-c",mount_command).run()?;
            }
            else {
                cmd!(&self.ltfs, "-o", format!("devname={drive}", drive=self.ltfs_drive.to_string_lossy()), &self.ltfs_path).run()?;
            }
            if ! self.is_mounted()? {
                Err(eyre!("path {ltfs_path} was not mounted", ltfs_path=self.ltfs_path.display()))?;
            }
        }
        else {
            debug!("LTFS is already mounted");
        }
        Ok(())
    }

    fn format_ltfs(&self) -> Result<()> {
        info!("Running mkltfs commmand");
        let force = match &self.cmd {
            Some(Command::FormatLTFS(b)) => b.force,
            _ => false,
        };

        if self.is_mounted()? {
            Err(eyre!("Can't format mounted tape, please unmount first"))?
        }

        let output = if force {
            cmd!(&self.mkltfs, "-d", &self.ltfs_drive,"-f").unchecked().run()?
        }
        else {
            cmd!(&self.mkltfs, "-d", &self.ltfs_drive).unchecked().run()?
        };
        match output.status.code() {
            Some(16) => {
                debug!("mkltfs returned exit code 16, already formatted");
            }, // already formatted
            Some(0) => {
                debug!("mkltfs format succeeded");
            }, // format succeeded
            code => Err(eyre!("mkltfs command returned exit status {code:#?}"))?,
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
                            // if ltfs filesystem is mounted
                            if self.is_mounted()? {
                                debug!("Unmounting LTFS filesystem");
                                // attempt to unmount the tape
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
                                self.format_ltfs()?;
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

    fn init_db(&mut self) -> Result<()> {
        debug!("Called init_db");

        if ! self.db.is_file() && !self.create_db {
            Err(eyre!("Could not find metadata database: {db}. Try using --create-db or --db DB_NAME", db=self.db.display()))?
        }
        if let Some(tapes_db) = &self.tapes_db {
            if ! tapes_db.is_file() && !self.create_db {
                Err(eyre!("Could not find tapes metadata database: {tapes_db}", tapes_db=tapes_db.display()))?
            }
        }
        else {
            let mut tapes_db = self.db.clone();
            tapes_db.set_file_name("tapes.db");
            if ! tapes_db.is_file() && !self.create_db {
                Err(eyre!("Could not find tapes metadata database: {tapes_db}", tapes_db=tapes_db.display()))?
            }
            self.tapes_db = Some(tapes_db);
        }

        if let Some(tapes_db) = &self.tapes_db {
            debug!("Connecting to tapes db {tapes_db}", tapes_db=tapes_db.display());
            {
                let conn = Connection::open(tapes_db)?;
                conn.execute_batch(r#"PRAGMA journal_mode=WAL;"#)?;
            }
            {
                let conn = Connection::open(tapes_db)?;
                conn.execute_batch(r#"PRAGMA journal_mode=WAL;"#)?;

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
        }

        debug!("Connecting to db {db}", db=self.db.display());
        {
            let conn = Connection::open(&self.db)?;
            conn.execute_batch(r#"PRAGMA journal_mode=WAL;"#)?;
        }
        {
            let mut conn = Connection::open(&self.db)?;
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
            create index if not exists file_path_mtime_size_idx on file (path,mtime,size,filetype);
            create table if not exists backup2file (
                backup_id integer not null references backup(id),
                file_id integer not null references file(id),
                uid integer not null,
                gid integer not null,
                mode integer not null,
                is_deleted integer not null
            );
            create index if not exists backup2file_backup_id_idx on backup2file (backup_id);
            create index if not exists backup2file_file_id_idx on backup2file (file_id);
            "#;
            debug!("Running sql:\n{sql}\n");
            conn.execute_batch(sql)?;

            debug!("Transfer tapes_db tapes records to db tape table");
            {
                let tapes = self.tape_status()?;
                let tx = conn.transaction()?;
                {
                    let mut stmt = tx.prepare(r#"insert into tape (id,label,is_full,date_added) values (:id,:label,:is_full,:date_added) on conflict(label) do nothing"#)?;
                    for tape in &tapes {
                        debug!("Inserting tape record {tape:#?}");
                        stmt.execute(serde_rusqlite::to_params_named_with_fields(&tape, &["id","label","is_full","date_added"])?.to_slice().as_slice())?;
                    }
                }
                tx.commit()?;
            }
        }
        Ok(())
    }

    fn backup_files(&mut self) -> Result<()> {
        debug!("Called backup_files");
        // initialize the backup set's sqlite database
        self.init_db()?;

        let backup_cmd = match &self.cmd {
            Some(Command::Backup(b)) => b,
            _ => Err(eyre!("Couldn't match backup subcommand!"))?,
        };

        debug!("Connecting to db {db}", db=self.db.display());
        let mut conn = Connection::open(&self.db)?;

        let mut tape = if ! backup_cmd.tape_full_test {
            // make sure a non-full tape is loaded, and create any missing tape db records
            Some(self.ready_storage()?)
        }
        else {
            // if --tape-full-test was passed, operate on the current tape 
            // even if it's full
            self.loader_status()?.iter().filter(|t| t.storage_type == StorageType::Drive && t.status == StorageStatus::Full).map(|t| t.clone()).next()
        };

        // get the tape db records
        let mut tapes = LinkedHashMap::<String,Tape>::new();
        for tape in self.tape_status()? {
            tapes.insert(tape.label.clone(), tape);
        }
        debug!("got tapes records: {tapes:#?}");

        // get the tape db record for the current tape
        let mut tape_record = if let Some(tape) = &tape {
            if let Some(label) = &tape.label {tapes.get(label).map(|t| t.clone())} else {None}
        } else {None};
        debug!("tape={tape:#?}, tape_record={tape_record:#?}");

        if ! backup_cmd.dry_run && tape_record.is_none() {
            Err(eyre!("Tape record is None {tape_record:#?}!, tape={tape:#?}"))?
        }

        // get the parent backup ID
        let mut parent_backup_id = Option::<i64>::None;
        if ! backup_cmd.full {
            let sql = r#"select id from backup order by date_created desc nulls last limit 1"#;
            debug!("Running query: {sql}");
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                parent_backup_id = Some(row.get(0)?);
            }
        }
        debug!("got parent backup id {parent_backup_id:#?}");

        // build the full backup in-memory index
        info!("Building index");
        let mut backup_index = LinkedHashMap::<PathBuf, FullBackup>::new();
        {
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
                where not b2f.is_deleted
                order by pb.depth desc, f.id
            "#;
            debug!("Running sql query:\n{sql}\n");
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query(params![parent_backup_id])?;
            let mut rows_iter = serde_rusqlite::from_rows::<FullBackup>(rows);
            let mut idxcount = 0u64;
            while let Some(full_backup) = rows_iter.next() {
                let full_backup = full_backup?;
                //trace!("Handling full_backup record: {full_backup:#?}");
                backup_index.insert(full_backup.path.clone(), full_backup);
                idxcount += 1;
                if idxcount % 1_000_000 == 0 {
                    info!("Building index with {idxcount} records...");
                }
            }
        }
        info!("Built index with {} records", backup_index.len());

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

        let mut count = 0u64;
        let mut tape_full = self.is_tape_full()?;
        let mut old_dir = Vec::<(PathBuf, i64)>::new();
        // use the glob patterns to produce a list of files
        for g in &backup_cmd.patterns {
            for entry in glob::glob(g.as_str())? {
                match &entry {
                    Ok(entry) => {
                        for direntry in WalkDir::new(&entry) {
                            match &direntry {
                                Ok(entry) => {
                                    let path = entry.path().to_owned();
                                    let mut paths = vec![path];
                                    'PATHS:
                                    while let Some(path) = paths.pop() {

                                        // The tape has become full. 
                                        // we need to re-mount the filesystem to ensure we're using the most 
                                        // recent index that was able to be written. Then we need to
                                        // scan the ltfs filesystem to verify which files actually
                                        // were transferred successfully.
                                        // Any files that were not included in the last written ltfs index
                                        // partition will now be missing, and should be deleted from the database.
                                        if ! backup_cmd.dry_run && (tape_full || backup_cmd.tape_full_test) {
                                            info!("The tape became full, re-mounting LTFS and verifying files");
                                            self.unmount_tape()?;
                                            self.mount_tape()?;
                                            info!("Re-mounting tape completed, now verifying files on tape");

                                            let tx = conn.transaction()?;
                                            let mut deleted_count = 0u64;
                                            if let Some(backup) = &backup {
                                                let sql = r#"
                                                select f.id, f.path, f.mtime, f.size, b2f.uid, b2f.gid, b2f.mode, f.filetype, f.symlink_target
                                                from backup2file b2f
                                                join file f
                                                on b2f.file_id=f.id
                                                where b2f.backup_id=?1
                                                and f.tape_id=?2
                                                order by f.path nulls last
                                                "#;
                                                debug!("Running sql: {sql}");
                                                let mut stmt = tx.prepare(sql)?;
                                                let backup_id = backup.id;
                                                let tape_record = &tape_record.ok_or_else(|| eyre!("no tape record!"))?;
                                                let rows = stmt.query(params![backup_id, tape_record.id])?;
                                                let mut rows_iter = serde_rusqlite::from_rows::<NewFile>(rows);
                                                let mut recopy = Vec::<PathBuf>::new();
                                                while let Some(new_file) = rows_iter.next() {
                                                    let new_file = new_file?;
                                                    trace!("new_file={new_file:#?}");
                                                    let new_file_path = new_file.path.strip_prefix("/").unwrap_or_else(|_| new_file.path.as_path());
                                                    trace!("new_file_path={new_file_path:#?}");

                                                    let path = PathBuf::from(stfu8::encode_u8_pretty([
                                                        &dest.as_path(),
                                                        &new_file_path,
                                                    ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                                                    trace!("path={path}", path=path.to_string_lossy());
                                                    let mut file_exists = false;
                                                    if path.exists() {
                                                        match new_file.filetype {
                                                            FileType::File => {
                                                                trace!("is a file");
                                                                if !path.is_symlink() && path.is_file() {
                                                                    trace!("found file {path}", path=path.to_string_lossy());
                                                                    if let Ok(md) = std::fs::metadata(&path) {
                                                                        if md.size() == new_file.size && md.mtime() == new_file.mtime {
                                                                            trace!("sizes and mtimes match, files are the same");
                                                                            file_exists = true;
                                                                        }
                                                                        else {
                                                                            trace!("sizes and mtimes do NOT match, files are different: tape_size={}, db_size={}, tape_mtime={}, db_mtime={}", md.size(), new_file.size, md.mtime(), new_file.mtime);
                                                                        }
                                                                    }
                                                                }
                                                            },
                                                            FileType::Directory => {
                                                                trace!("is a directory");
                                                                file_exists = !path.is_symlink() && path.is_dir();
                                                            },
                                                            FileType::Symlink => {
                                                                trace!("is a symlink");
                                                                if let Some(symlink_target) = &new_file.symlink_target {
                                                                    trace!("symlink_target={symlink_target}", symlink_target=symlink_target.to_string_lossy());
                                                                    if path.is_symlink() {
                                                                        trace!("found symlink at path");
                                                                        let target = std::fs::read_link(&path)?;
                                                                        if target == *symlink_target {
                                                                            trace!("targets match: {target}", target=target.to_string_lossy());
                                                                            file_exists = true;
                                                                        }
                                                                        else {
                                                                            trace!("targets do NOT match: {} vs {}", target.to_string_lossy(), symlink_target.to_string_lossy());
                                                                        }
                                                                    }
                                                                } 
                                                                else {
                                                                    Err(eyre!("Symlink {path} is missing symlink_target!", path=path.to_string_lossy()))?
                                                                }
                                                            },
                                                        };
                                                    }
                                                    // remove db records for missing files
                                                    if ! file_exists {
                                                        trace!("File {path} does not exist, queueing for deletion", path=path.to_string_lossy());
                                                        trace!("Removing records for {new_file:#?}");
                                                        let sql = r#"delete from backup2file where file_id=?1"#;
                                                        trace!("Running sql delete:\n{sql}\n");
                                                        tx.execute(sql, params![new_file.id])?; 
                                                        let sql = r#"delete from file where id=?1"#;
                                                        trace!("Running sql delete:\n{sql}\n");
                                                        tx.execute(sql, params![new_file.id])?; 
                                                        deleted_count += 1;

                                                        recopy.push(path);
                                                    }
                                                }
                                                // re-copy the files
                                                recopy.extend(paths);
                                                paths = recopy;
                                            }
                                            tx.commit()?;
                                            info!("Verification complete, removed {count} files from the database", count=deleted_count);

                                            // load the new tape
                                            tape = Some(self.ready_storage()?);

                                            // update the tape db records
                                            tapes.clear();
                                            for tape in self.tape_status()? {
                                                tapes.insert(tape.label.clone(), tape);
                                            }
                                            debug!("updated tapes records: {tapes:#?}", tapes=tapes);

                                            // update the tape record
                                            let label = if let Some(tape) = &tape {
                                                if let Some(label) = &tape.label 
                                                    {Some(label)} else {None}
                                                } 
                                                else {None}
                                                .ok_or_else(|| eyre!("No label found for tape!"))?;
                                            debug!("label={tape:#?}");
                                            tape_record = tapes.get(label).map(|t| t.clone());

                                            // reset the tape_full flag
                                            tape_full = self.is_tape_full()?;

                                            // exit the backup command if this was a tape full test
                                            if backup_cmd.tape_full_test {
                                                info!("Tape full test completed, exiting backup");
                                                return Ok(());
                                            }
                                            if tape_full {
                                                Err(eyre!("Tape is full!"))?
                                            }
                                        }

                                        // ignore any entries that are not one of: directory, file, symlink
                                        if !path.is_dir() && !path.is_file() && !path.is_symlink() {
                                            continue 'PATHS;
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
                                                continue 'PATHS;
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
                                                continue 'PATHS;
                                            }
                                        }
                                        // check exclude filters
                                        if !backup_cmd.exclude_regex.is_empty() {
                                            for e in &backup_cmd.exclude_regex {
                                                if e.is_match(path.as_os_str().as_bytes()) {
                                                    continue 'PATHS;
                                                }
                                            }
                                        }
                                        if !backup_cmd.exclude.is_empty() {
                                            for e in &backup_cmd.exclude {
                                                if e.matches_path(&path) {
                                                    continue 'PATHS;
                                                }
                                            }
                                        }

                                        // get file metadata
                                        let metadata = match std::fs::symlink_metadata(&path) {
                                            Ok(m) => m,
                                            Err(e) => {
                                                warn!("Could not extract metadata for file {}, skipping: {e}", path.to_string_lossy());
                                                continue 'PATHS;
                                            }
                                        };
                                        let filetype = if path.is_symlink() {FileType::Symlink} else if path.is_dir() {FileType::Directory} else {FileType::File};

                                        // get symlink target if applicable
                                        let target = if path.is_symlink() {
                                            match std::fs::read_link(&path) {
                                                Ok(t) => Some(t),
                                                Err(e) => {
                                                    warn!("Could not read target of symlink {}: {e}", path.to_string_lossy());
                                                    continue 'PATHS;
                                                },
                                            }
                                        }
                                        else { None };

                                        // if file exists in backup but metadata has changed, only insert a new backup2file record
                                        //if let Some(parent_record) = backup_index.remove(&(path.clone(), metadata.mtime(), metadata.size(), filetype as u8, target.clone())) {
                                        if let Some(parent_record) = backup_index.remove(&path) {
                                            if metadata.mtime() == parent_record.mtime && metadata.size() == parent_record.size && filetype == parent_record.filetype && target == parent_record.symlink_target 
                                            {
                                                if parent_record.uid == metadata.uid() && parent_record.gid == metadata.gid() && parent_record.mode == metadata.mode() {
                                                    //trace!("Skipping file {}, already backed up", path.to_string_lossy());
                                                    continue 'PATHS;
                                                }
                                                // add file and backup2file records to the database
                                                if let Some(backup) = &backup {
                                                    let tx = conn.transaction()?;
                                                    {
                                                        trace!("Updating metadata for {}", path.to_string_lossy());
                                                        let mut b2f_stmt = tx.prepare(r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (:backup_id,:file_id,:uid,:gid,:mode,:is_deleted)"#)?;
                                                        b2f_stmt.execute(serde_rusqlite::to_params_named(&Backup2File {
                                                            backup_id: backup.id,
                                                            file_id: parent_record.file_id,
                                                            uid: metadata.uid(),
                                                            gid: metadata.gid(),
                                                            mode: metadata.mode(),
                                                            is_deleted: false,
                                                        })?.to_slice().as_slice())?;
                                                    }
                                                    tx.commit()?;
                                                }
                                                count += 1;
                                                continue 'PATHS;
                                            }
                                        }

                                        trace!("Handling file {path}", path=path.to_string_lossy());
                                        let file_path = PathBuf::from(path.parent().unwrap_or(PathBuf::from(".").as_path()));
                                        let file_path_stripped = file_path.strip_prefix("/").unwrap_or_else(|_| file_path.as_path());
                                        let file_name = match path.file_name() {
                                            Some(n) => n,
                                            None => {
                                                warn!("Could not parse file name of file {}", path.to_string_lossy());
                                                continue 'PATHS;
                                            },
                                        };

                                        let dest_path = PathBuf::from(stfu8::encode_u8_pretty([
                                            &dest.as_path(), 
                                            &file_path_stripped,
                                        ].iter().collect::<PathBuf>().as_os_str().as_bytes()));

                                        // make sure dest path exists
                                        trace!("Create parent directories: {}", dest_path.to_string_lossy());
                                        if let Err(e) = std::fs::create_dir_all(&dest_path) {
                                            if self.is_tape_full()? {
                                                tape_full = true;
                                                paths.push(path);
                                                continue 'PATHS;
                                            }
                                            else {
                                                Err(e)?
                                            }
                                        }

                                        // check if we need to set old_dir's mtime
                                        if ! backup_cmd.dry_run {
                                            while ! old_dir.is_empty() {
                                                if let Some((last_old_dir, mtime)) = old_dir.last() {
                                                    match file_path.strip_prefix(last_old_dir) {
                                                        Ok(_) => { 
                                                            // old_dir is still a parent of the current file's path
                                                            break;
                                                        },
                                                        Err(_) => {
                                                            trace!("old_dir changed from {old_dir} to {dest_path}, setting mtime on old_dir", old_dir=last_old_dir.to_string_lossy(), dest_path=dest_path.to_string_lossy());
                                                            let mtime = FileTime::from_unix_time(*mtime, 0);
                                                            if let Err(e) = filetime::set_file_times(&last_old_dir, mtime.clone(), mtime.clone()) {
                                                                if self.is_tape_full()? {
                                                                    tape_full = true;
                                                                    paths.push(path);
                                                                    continue 'PATHS;
                                                                }
                                                                else {
                                                                    Err(e)?
                                                                }
                                                            }
                                                            old_dir.pop();
                                                        },
                                                    }
                                                }
                                            }
                                        }
                                        // handle file types
                                        match filetype {
                                            FileType::Directory => {
                                                trace!("is a directory");
                                                let destdir = PathBuf::from(stfu8::encode_u8_pretty([
                                                    &dest_path,
                                                    &PathBuf::from(file_name),
                                                ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                                                trace!("destdir={destdir}", destdir=destdir.to_string_lossy());

                                                info!("{path}/ -> {destdir}/", path=path.to_string_lossy(), destdir=destdir.to_string_lossy());
                                                if ! backup_cmd.dry_run {
                                                    // create the directory
                                                    trace!("create directory {destdir}", destdir=destdir.to_string_lossy());
                                                    if let Err(e) = std::fs::create_dir(&destdir) {
                                                        if self.is_tape_full()? {
                                                            tape_full = true;
                                                            paths.push(path);
                                                            continue 'PATHS;
                                                        }
                                                        else {
                                                            Err(e)?
                                                        }
                                                    }
                                                }
                                                // set old_dir so we can set the mtime later
                                                old_dir.push((destdir.clone(), metadata.mtime()));
                                                trace!("push old_dir: {old_dir:#?}");
                                            },
                                            FileType::Symlink => {
                                                trace!("is a symlink");
                                                // create the symlink
                                                let dest_file = PathBuf::from(stfu8::encode_u8_pretty([
                                                    &dest_path, 
                                                    &PathBuf::from(file_name),
                                                ].iter().collect::<PathBuf>().as_os_str().as_bytes()));
                                                trace!("dest_file={dest_file}", dest_file=dest_file.to_string_lossy());

                                                if let Some(target) = &target {
                                                    info!("{path} -> {dest_file} ({target})", path=path.to_string_lossy(), dest_file=dest_file.to_string_lossy(), target=target.to_string_lossy());
                                                    if ! backup_cmd.dry_run {
                                                        trace!("symlink {target} to {dest_file}", target=target.to_string_lossy(), dest_file=dest_file.to_string_lossy());
                                                        if let Err(e) = std::os::unix::fs::symlink(&target, &dest_file) {
                                                            if self.is_tape_full()? {
                                                                tape_full = true;
                                                                paths.push(path);
                                                                continue 'PATHS;
                                                            }
                                                            else {
                                                                Err(e)?
                                                            }
                                                        }
                                                        // set the symlinks's mtime
                                                        trace!("set symlink {dest_file} mtime", dest_file=dest_file.to_string_lossy());
                                                        let mtime = FileTime::from_unix_time(metadata.mtime(), 0);
                                                        if let Err(e) = filetime::set_symlink_file_times(&dest_file, mtime.clone(), mtime.clone()) {
                                                            if self.is_tape_full()? {
                                                                tape_full = true;
                                                                paths.push(path);
                                                                continue 'PATHS;
                                                            }
                                                            else {
                                                                Err(e)?
                                                            }
                                                        }
                                                    }
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

                                                info!("{path} -> {dest_file}", path=path.to_string_lossy(), dest_file=dest_file.to_string_lossy());
                                                if ! backup_cmd.dry_run {
                                                    trace!("copy {file_path} to {tmp_file}", file_path=path.to_string_lossy(), tmp_file=tmp_file.to_string_lossy());
                                                    if let Err(e) = std::fs::copy(&path, &tmp_file) {
                                                        if self.is_tape_full()? {
                                                            tape_full = true;
                                                            paths.push(path);
                                                            continue 'PATHS;
                                                        }
                                                        else if !path.exists() {
                                                            error!("File {} no longer exists, skipping backup!", path.to_string_lossy());
                                                            continue 'PATHS;
                                                        }
                                                        else {
                                                            error!("File {} could not be copied, skipping!: {e}", path.to_string_lossy());
                                                            continue 'PATHS;
                                                        }
                                                    }
                                                    // set the file's mtime
                                                    trace!("set mtime on {tmp_file}", tmp_file=tmp_file.to_string_lossy());
                                                    let mtime = FileTime::from_unix_time(metadata.mtime(), 0);
                                                    if let Err(e) = filetime::set_file_times( &tmp_file, mtime.clone(), mtime.clone()) {
                                                        if self.is_tape_full()? {
                                                            tape_full = true;
                                                            paths.push(path);
                                                            continue 'PATHS;
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
                                                            paths.push(path);
                                                            continue 'PATHS;
                                                        }
                                                        else {
                                                            Err(e)?
                                                        }
                                                    }
                                                }
                                            },
                                        };
                                        // add file and backup2file records to the database
                                        if ! backup_cmd.dry_run {
                                            trace!("Adding file and backup2file records to database");
                                            let tx = conn.transaction()?;
                                            if let (Some(backup), Some(tape_record)) = (&backup, &tape_record) {
                                                let sql = r#"insert into file (tape_id,path,mtime,size,filetype,symlink_target) values (:tape_id,:path,:mtime,:size,:filetype,:symlink_target) returning *"#;
                                                let file = File{
                                                    id: 0,
                                                    tape_id: tape_record.id,
                                                    path: path.clone(),
                                                    mtime: metadata.mtime(),
                                                    size: metadata.size(),
                                                    filetype: filetype,
                                                    symlink_target: target,
                                                };
                                                trace!("Running sql:\n{sql}\nto insert file {file:#?}");
                                                let mut file_stmt = tx.prepare(sql)?;
                                                // write file and backup2file db records
                                                let rows = file_stmt.query(serde_rusqlite::to_params_named_with_fields(&file, &["tape_id","path","mtime","size","filetype","symlink_target"])?.to_slice().as_slice())?;

                                                let mut rows_iter = serde_rusqlite::from_rows::<File>(rows);
                                                if let Some(file_record) = rows_iter.next() {
                                                    let file_record = file_record?;
                                                    trace!("query returned file record: {file_record:#?}");
                                                    let backup2file = Backup2File {
                                                        backup_id: backup.id,
                                                        file_id: file_record.id,
                                                        uid: metadata.uid(),
                                                        gid: metadata.gid(),
                                                        mode: metadata.mode(),
                                                        is_deleted: false,
                                                    };
                                                    let sql = r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (:backup_id,:file_id,:uid,:gid,:mode,:is_deleted)"#;
                                                    trace!("Running sql:\n{sql}\nto insert back2file record: {backup2file:#?} ");
                                                    let mut b2f_stmt = tx.prepare(sql)?;
                                                    b2f_stmt.execute(serde_rusqlite::to_params_named(&backup2file)?.to_slice().as_slice())?;
                                                }
                                            }
                                            else {
                                                Err(eyre!("backup record {backup:#?} and/or tape record {tape_record:#?} empty!"))?
                                            }
                                            tx.commit()?;
                                            trace!("Committed");
                                            count += 1;
                                        }
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

        // check if we need to set old_dir's mtime
        if ! backup_cmd.dry_run {
            while let Some((old_dir, mtime)) = old_dir.pop() {
                let mtime = FileTime::from_unix_time(mtime, 0);
                if let Err(e) = filetime::set_file_times(&old_dir, mtime.clone(), mtime.clone()) {
                    if self.is_tape_full()? {
                        warn!("Could not set mtime on parent directories because tape became full!");
                        break;
                    }
                    else {
                        Err(e)?
                    }
                }
            }
        }

        // add records for deleted files
        if let Some(backup) = &backup {
            let tx = conn.transaction()?;
            {
                let mut last_path = Option::<PathBuf>::None;
                for (_, full_backup) in &backup_index {
                    if let Some(last_path) = &last_path {
                        if full_backup.path.starts_with(last_path) {
                            trace!("Path {path} is in already deleted directory {last_path}, not creating deletion record", path=full_backup.path.to_string_lossy(), last_path=last_path.to_string_lossy());
                            continue;
                        }
                    }
                    if full_backup.filetype == FileType::Directory {
                        last_path = Some(full_backup.path.clone());
                    }

                    trace!("Adding deleted record for file {}", full_backup.path.to_string_lossy());
                    let mut b2f_stmt = tx.prepare(r#"insert into backup2file (backup_id,file_id,uid,gid,mode,is_deleted) values (:backup_id,:file_id,:uid,:gid,:mode,:is_deleted)"#)?;
                    b2f_stmt.execute(serde_rusqlite::to_params_named(&Backup2File {
                        backup_id: backup.id,
                        file_id: full_backup.file_id,
                        uid: full_backup.uid,
                        gid: full_backup.gid,
                        mode: full_backup.mode,
                        is_deleted: true,
                    })?.to_slice().as_slice())?;
                    count += 1;
                }
            }
            tx.commit()?;
        }

        if ! backup_cmd.dry_run {
            if let Some(backup) = &backup {
                // set the backup complete flag in the db
                if count > 0 {
                    let sql = r#"update backup set is_complete=true where id=?1"#;
                    debug!("Running sql update:\n{sql}\n");
                    conn.execute(sql, params![backup.id])?;
                }
                // if no file records for this backup, remove the backup record
                else {
                    let sql = r#"delete from backup where id=?1"#;
                    debug!("Running sql delete:\n{sql}\n");
                    conn.execute(sql, params![backup.id])?;
                    info!("No files have changed since last backup. Exiting program.");
                }
            }
        }
        info!("Backup complete");
        Ok(())
    }

    fn restore_files(&mut self) -> Result<()> {
        debug!("Running restore_files");
        self.init_db()?;

        let mut list_tapes = LinkedHashMap::<String,ListTape>::new();

        let restore_cmd = match &self.cmd {
            Some(Command::Restore(r)) => r,
            _ => Err(eyre!("Couldn't match restore subcommand!"))?,
        };
        if ! restore_cmd.restore_path.is_dir() {
            Err(eyre!("Could not find restore path: {restore_path}", restore_path=restore_cmd.restore_path.to_string_lossy()))?
        }
        debug!("Using restore_path: {}", restore_cmd.restore_path.to_string_lossy());

        debug!("Connecting to db {db}", db=&self.db.to_string_lossy());
        let mut conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<i64,Tape>::new();
        for tape in self.tape_status()? {
            tapes.insert(tape.id, tape);
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

            // get the complete set of backup records for reference
            let mut backups = LinkedHashMap::<i64, Backup>::new();
            {
                let sql = format!(r#" select * from backup "#);
                debug!("Running query: {sql}");
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query(params![])?;
                let mut rows_iter = serde_rusqlite::from_rows::<Backup>(rows);
                while let Some(backup_record) = rows_iter.next() {
                    let backup_record = backup_record?;
                    backups.insert(backup_record.id, backup_record);
                }
            }

            // get the complete list of fullbackup file records from the db
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
              select backup_id, uid, gid, mode, is_deleted, file_id, tape_id, path, mtime, size, filetype, symlink_target
              from (
                select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype, f.symlink_target, 
                  f.id as file_id, pb.depth,
                  row_number() over (partition by f.path order by pb.depth) as rank
                from parent_backup pb
                join backup2file b2f
                  on b2f.backup_id = pb.id
                join file f
                  on b2f.file_id=f.id) s
              where rank=1
              order by file_id
            "#;
            debug!("Running sql query:\n{sql}\n");
            let mut deleted_path = Option::<PathBuf>::None;
            let mut deleted_files = Vec::<FullBackup>::new();
            let mut old_tape_id = Option::<i64>::None;
            let mut old_dir = Vec::<(PathBuf, i64)>::new();
            {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query([&backup.id])?;
                let mut rows_iter = serde_rusqlite::from_rows::<FullBackup>(rows);
                while let Some(full_backup) = rows_iter.next() {
                    let full_backup = full_backup?;
                    trace!("Handling full_backup record: {full_backup:#?}");

                    if let Some(deleted_path) = &deleted_path {
                        if full_backup.path.starts_with(deleted_path) {
                            trace!("Path {path} is in already deleted directory {deleted_path}, not restoring path", path=full_backup.path.to_string_lossy(), deleted_path=deleted_path.to_string_lossy());
                            continue;
                        }
                    }
                    if full_backup.is_deleted {
                        if full_backup.filetype == FileType::Directory {
                            deleted_path = Some(full_backup.path.clone());
                        }
                        trace!("Path {} is marked as deleted, skipping", full_backup.path.to_string_lossy());
                        continue;
                    }

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

                    let backup = backups.get(&full_backup.backup_id).
                        ok_or_else(|| eyre!("Could not find backup record for id {id}", id=full_backup.backup_id))?;

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

                    let full_backup_path = full_backup.path.strip_prefix("/").unwrap_or_else(|_| full_backup.path.as_path());
                    let source = PathBuf::from(OsString::from_vec(stfu8::decode_u8(&[
                        &source_path.as_path(),
                        &full_backup_path,
                    ].iter().collect::<PathBuf>().to_string_lossy().to_string())?));
                    let dest = [
                        &restore_cmd.restore_path.as_path(),
                        &full_backup_path,
                    ].iter().collect::<PathBuf>();
                    trace!("full_backup_path={full_backup_path}", full_backup_path=full_backup_path.to_string_lossy());

                    if let Some(parent) = dest.parent() {
                        // set the mtime of the previous directory if it's done
                        // being written
                        while ! old_dir.is_empty() {
                            if let Some((od, mtime)) = old_dir.last() {
                                match parent.strip_prefix(&od) {
                                    Ok(_) => {
                                        break;
                                    },
                                    Err(_) => {
                                        trace!("set mtime of old_dir {od} to {mtime}", od=od.to_string_lossy());
                                        let filetime = FileTime::from_unix_time(*mtime, 0);
                                        if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                                            filetime::set_file_times(od, filetime.clone(), filetime.clone())?;
                                        }
                                        old_dir.pop();
                                    },
                                }
                            }
                        }
                        trace!("create parent directories for {}", parent.to_string_lossy());
                        if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                            std::fs::create_dir_all(parent)?;
                        }
                    }
                    // transfer the file
                    trace!("Checking type of full_backup {full_backup:#?}");
                    match full_backup.filetype {
                        FileType::Directory => {
                            trace!("is a directory");
                            if ! restore_cmd.list_tapes {
                                info!("{source}/ -> {dest}/", source=source.to_string_lossy(), dest=dest.to_string_lossy());
                            }
                            let mut mtime = full_backup.mtime;
                            let md = std::fs::metadata(&dest).ok(); 
                            if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                                trace!("mkdir {} with mode {}", dest.to_string_lossy(), full_backup.mode);
                                match nix::unistd::mkdir(&dest, Mode::from_bits_truncate(full_backup.mode)) {
                                    Ok(_) | Err(nix::errno::Errno::EEXIST) => (),
                                    Err(e) => Err(e)?,
                                }
                                trace!("chown {}", dest.to_string_lossy());
                                nix::unistd::chown(&dest, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                                if let Some(md) = md {
                                    let current_mtime = FileTime::from_last_modification_time(&md);
                                    mtime = std::cmp::max(mtime, current_mtime.unix_seconds());
                                }
                            }
                            old_dir.push((dest, mtime));
                        },
                        FileType::Symlink => {
                            trace!("is a symlink");
                            if dest.exists() && restore_cmd.noclobber {
                                continue;
                            }
                            if ! dest.is_symlink() && dest.is_dir() {
                                error!("Destination {dest} is a directory instead of a symlink!", dest=dest.to_string_lossy());
                                continue;
                            }
                            let target = full_backup.symlink_target.ok_or_else(|| eyre!("Symlink target is null!"))?;
                            if ! restore_cmd.list_tapes {
                                info!("{source} -> {dest} ({target})", source=source.to_string_lossy(), dest=dest.to_string_lossy(), target=target.to_string_lossy());
                            }
                            if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                                if dest.exists() {
                                    trace!("remove destination {}", dest.to_string_lossy());
                                    std::fs::remove_file(&dest)?;
                                }
                                
                                trace!("create symlink {} {}", dest.to_string_lossy(), target.to_string_lossy());
                                if let Err(e) = std::os::unix::fs::symlink(&target, &dest) {
                                    if !dest.exists() || std::fs::read_link(&dest)? != dest {
                                        Err(e)?
                                    }
                                }

                                trace!("chown {}", dest.to_string_lossy());
                                nix::unistd::chown(&dest, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                                trace!("set mtime for  {}", dest.to_string_lossy());
                                let filetime = FileTime::from_unix_time(full_backup.mtime, 0);
                                filetime::set_symlink_file_times(&dest, filetime.clone(), filetime.clone())?;
                            }
                        },
                        FileType::File => {
                            trace!("{} is a file", dest.to_string_lossy());
                            let mut is_required = true;
                            if dest.exists() {
                                if restore_cmd.noclobber {
                                    debug!("File {} already exists, skipping because noclobber is set", dest=dest.to_string_lossy());
                                    continue;
                                } 
                                if ! dest.is_symlink() && dest.is_dir() {
                                    error!("Destination {dest} is a directory instead of a file!", dest=dest.to_string_lossy());
                                    continue;
                                }

                                // if mtime and size is the same, skip copying the file
                                trace!("check if file {} is already up to date", dest.to_string_lossy());
                                let attr = std::fs::metadata(&dest)?;
                                if dest.is_file() && attr.mtime() == full_backup.mtime && attr.size() == full_backup.size {
                                    trace!("file {} is already up to date", dest.to_string_lossy());
                                    if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                                        // set uid:gid
                                        trace!("chown {}", dest.to_string_lossy());
                                        nix::unistd::chown(&dest, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                                        // set mode
                                        trace!("chmod {}", dest.to_string_lossy());
                                        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(full_backup.mode))?;
                                    }
                                    is_required = false;
                                }
                                else {
                                    if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                                        // remove existing destination file to break any hard links
                                        trace!("remove existing file {}", dest.to_string_lossy());
                                        std::fs::remove_file(&dest)?;
                                    }
                                }
                            }
                            if is_required {
                                trace!("Tape {:#?} is required due to file {}", 
                                    tapes.get(&full_backup.tape_id).map(|t| t.label.clone()),
                                    dest.to_string_lossy(),
                                );
                            }
                            if let Some(dest_path) = dest.parent() {
                                if let Some(file_name) = dest.file_name() {
                                    // generate temp file name
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
                                    trace!("tmp_file = {}", tmp_file.to_string_lossy());

                                    if ! restore_cmd.list_tapes {
                                        info!("{source} -> {dest}", source=source.to_string_lossy(), dest=dest.to_string_lossy());
                                    }

                                    // make sure correct tape is loaded and mounted
                                    if old_tape_id != Some(full_backup.tape_id) {
                                        // get loader status
                                        if let Some(tape) = tapes.get(&full_backup.tape_id) {
                                            let ls = self.loader_status()?;
                                            if let Some(se) = ls.iter().filter(|se| if let Some(label) = &se.label {*label==tape.label} else {false}).next() {
                                                if restore_cmd.list_tapes {
                                                    if let Some(lt) = list_tapes.get_mut(&tape.label) {
                                                        if is_required && !lt.is_required {
                                                            lt.is_required = is_required;
                                                        }
                                                    }
                                                    else {
                                                        list_tapes.insert(tape.label.clone(), ListTape {
                                                            label: tape.label.clone(),
                                                            is_loaded: true,
                                                            is_required: is_required,
                                                        });
                                                    }
                                                }
                                                else if is_required {
                                                    if se.storage_type == StorageType::Drive {
                                                        debug!("Tape {label} is already loaded", label=&tape.label);
                                                        if ! self.is_mounted()? {
                                                            self.mount_tape()?;
                                                        }
                                                    }
                                                    else {
                                                        debug!("Loading tape {label}", label=&tape.label);
                                                        self.unmount_tape()?;
                                                        self.load_tape(se.index)?;
                                                        self.mount_tape()?;
                                                    }
                                                }
                                            }
                                            else {
                                                if restore_cmd.list_tapes {
                                                    if let Some(lt) = list_tapes.get_mut(&tape.label) {
                                                        if is_required && !lt.is_required {
                                                            lt.is_required = is_required;
                                                        }
                                                    }
                                                    else {
                                                        list_tapes.insert(tape.label.clone(), ListTape {
                                                            label: tape.label.clone(),
                                                            is_loaded: false,
                                                            is_required: is_required,
                                                        });
                                                    }
                                                }
                                                else {
                                                    Err(eyre!("Could not find tape with label {label} in loader!", label=tape.label))?
                                                }
                                            }
                                        }
                                        else {
                                            Err(eyre!("Could not find database record for tape with id {label}!", label=full_backup.tape_id))?
                                        }
                                        old_tape_id = Some(full_backup.tape_id);
                                    }

                                    // if --list-tapes was specified, skip the next section, since the tape
                                    // won't actually be loaded
                                    // if is_required is false, then skip as well, since the file is already
                                    // present
                                    if restore_cmd.list_tapes || ! is_required {
                                        continue;
                                    }

                                    // make sure the source file exists
                                    if !source.exists() {
                                        if restore_cmd.verify_db {
                                            info!("File {source} does not exist, queueing for deletion", source=source.to_string_lossy());
                                            deleted_files.push(full_backup);
                                        }
                                        else {
                                            Err(eyre!("Error: File {source} does not exist on tape!", source=source.to_string_lossy()))?;
                                        }
                                    } 
                                    else if !source.is_file() {
                                        Err(eyre!("Error: File {source} exists on tape but is not a file!", source=source.to_string_lossy()))?;
                                    } 
                                    else {
                                        // check file size and checksums match
                                        let metadata = match std::fs::symlink_metadata(&source) {
                                            Ok(m) => m,
                                            Err(e) => {
                                                Err(eyre!("Could not extract metadata for file {}: {}", source.to_string_lossy(), e))?
                                            }
                                        };
                                        if metadata.size() != full_backup.size {
                                            Err(eyre!("Error: File {source} has size {msize} is different than database size {fbsize}", source=source.to_string_lossy(), msize=metadata.size(), fbsize=full_backup.size))?;
                                        }
                                        if metadata.mtime() != full_backup.mtime {
                                            Err(eyre!("Error: File {source} has mtime {mmtime} is different than database size {fbmtime}", source=source.to_string_lossy(), mmtime=metadata.mtime(), fbmtime=full_backup.mtime))?;
                                        }

                                        if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                                            // copy the file
                                            trace!("source={source}", source=source.to_string_lossy());
                                            trace!("tmp_file={tmp_file}", tmp_file=tmp_file.to_string_lossy());
                                            trace!("dest={dest}", dest=dest.to_string_lossy());
                                            trace!("copying {} to {}", source.to_string_lossy(), tmp_file.to_string_lossy());
                                            std::fs::copy(&source, &tmp_file)?;
                                            // set uid:gid
                                            trace!("chown {}", tmp_file.to_string_lossy());
                                            nix::unistd::chown(&tmp_file, Some(Uid::from_raw(full_backup.uid)), Some(Gid::from_raw(full_backup.gid)))?;
                                            // set mode
                                            trace!("chmod {}", tmp_file.to_string_lossy());
                                            std::fs::set_permissions(&tmp_file, std::fs::Permissions::from_mode(full_backup.mode))?;
                                            trace!("set mtime for {}", tmp_file.to_string_lossy());
                                            // set the file's mtime
                                            let mtime = FileTime::from_unix_time(full_backup.mtime, 0);
                                            filetime::set_file_times(&tmp_file, mtime.clone(), mtime.clone())?;
                                            // then move file into place
                                            trace!("move {} to {}", tmp_file.to_string_lossy(), dest.to_string_lossy());
                                            std::fs::rename(&tmp_file, dest)?;
                                        }
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
            }
            // set the mtime for the previous directory
            while let Some((old_dir, mtime)) = old_dir.pop() {
                let filetime = FileTime::from_unix_time(mtime, 0);
                if ! restore_cmd.dry_run && ! restore_cmd.list_tapes {
                    trace!("set mtime for old_dir {} to {}", old_dir.to_string_lossy(), mtime);
                    filetime::set_file_times(old_dir, filetime.clone(), filetime.clone())?;
                }
            }
            // remove files from db that are missing on tape
            if restore_cmd.verify_db {
                let tx = conn.transaction()?;
                for full_backup in &deleted_files {
                    info!("Removing records for {source}", source=full_backup.path.to_string_lossy());
                    let sql = r#"delete from backup2file where file_id=?1"#;
                    trace!("Running sql delete:\n{sql}\n");
                    tx.execute(sql, params![full_backup.file_id])?; 
                    let sql = r#"delete from file where id=?1"#;
                    trace!("Running sql delete:\n{sql}\n");
                    tx.execute(sql, params![full_backup.file_id])?;
                }
                tx.commit()?;
                info!("Verification complete, removed {count} files from the database", count=deleted_files.len());
            }
        }
        else {
            Err(eyre!("No backup found matching backup_id {backup_id:#?}", backup_id=restore_cmd.backup_id))?
        }

        if restore_cmd.list_tapes {
            self.init_pager()?;
            let mut csv_writer = csv::Writer::from_writer(std::io::stdout());
            for (_, lt) in list_tapes {
                csv_writer.serialize(lt)?;
            }
        }
        else {
            info!("Restore complete");
        }
        Ok(())
    }

    fn list_backups(&mut self) -> Result<()> {
        debug!("Running list_backups");
        self.init_db()?;

        let list_backups_cmd = match &self.cmd {
            Some(Command::ListBackups(l)) => l,
            _ => Err(eyre!("Couldn't match subcommand!"))?,
        };

        self.init_pager()?;

        let mut csv_writer = csv::Writer::from_writer(std::io::stdout());

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

            let mut rows_iter = serde_rusqlite::from_rows::<Backup>(rows);
            while let Some(backup) = rows_iter.next() {
                let backup = backup?;
                csv_writer.serialize(backup)?;
            }
        }
        Ok(())
    }

    fn list_files(&mut self) -> Result<()> {
        debug!("Running list_files");
        self.init_db()?;

        let list_files_cmd = match &self.cmd {
            Some(Command::ListFiles(l)) => l,
            _ => Err(eyre!("Couldn't match subcommand!"))?,
        };

        self.init_pager()?;

        let mut csv_writer = csv::Writer::from_writer(std::io::stdout());

        debug!("Connecting to database {db}", db=self.db.to_string_lossy());
        let conn = Connection::open(&self.db)?;

        // get the tape db records
        let mut tapes = LinkedHashMap::<i64,Tape>::new();
        for tape in self.tape_status()? {
            tapes.insert(tape.id, tape);
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
              select backup_id, uid, gid, mode, is_deleted, file_id, tape_id, path, mtime, size, filetype, symlink_target
              from (
                select b2f.backup_id, b2f.uid, b2f.gid, b2f.mode, b2f.is_deleted, b2f.file_id, f.tape_id, f.path, f.mtime, f.size, f.filetype, f.symlink_target, 
                  f.id as file_id, pb.depth,
                  row_number() over (partition by f.path order by pb.depth) as rank
                from parent_backup pb
                join backup2file b2f
                  on b2f.backup_id = pb.id
                join file f
                  on b2f.file_id=f.id) s
              where rank=1
              order by file_id
            "#;
            debug!("Running sql query:\n{sql}\n");
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query([&backup.id])?;
            let mut rows_iter = serde_rusqlite::from_rows::<FullBackup>(rows);
            csv_writer.write_record(&["mode","owner","group","size","mtime","tape_label","path","symlink_target"])?;
            let mut deleted_path = Option::<PathBuf>::None;
            while let Some(full_backup) = rows_iter.next() {
                let full_backup = full_backup?;

                if let Some(deleted_path) = &deleted_path {
                    if full_backup.path.starts_with(deleted_path) {
                        continue;
                    }
                }
                if full_backup.is_deleted {
                    if full_backup.filetype == FileType::Directory {
                        deleted_path = Some(full_backup.path.clone());
                    }
                    continue;
                }

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

    fn list_tapes(&mut self) -> Result<()> {
        debug!("Running list_tapes");
        self.init_db()?;
        let mut tapes = HashMap::<String,Tape>::new();
        for tape in self.tape_status()? {
            tapes.insert(tape.label.clone(), tape);
        }
        debug!("got tapes records: {tapes:#?}");

        let ls = self.loader_status()?;
        let mut tape_labels = BTreeSet::<String>::new();
        let mut loader_statuses = HashMap::<String,StorageElement>::new();
        for se in &ls {
            if let Some(l) = &se.label {
                tape_labels.insert(l.clone());
                loader_statuses.insert(l.clone(), se.clone());
            }
        }
        for (label, _) in &tapes {
            tape_labels.insert(label.clone());
        }
        debug!("tape_labels={tape_labels:#?}");
        debug!("loader_statuses={loader_statuses:#?}");

        self.init_pager()?;

        let mut csv_writer = csv::Writer::from_writer(std::io::stdout());
        for label in &tape_labels {
            debug!("label={label:#?}");
            let tape = tapes.get(label);
            debug!("tape={tape:#?}");
            let ls = loader_statuses.get(label);
            debug!("ls={ls:#?}");
            let ts = TapeStatus {
                label: label.clone(),
                is_full: tape.map(|t| t.is_full),
                date_added: tape.map(|t| t.date_added.clone()),
                storage_type: ls.map(|l| l.storage_type.clone()),
                index: ls.map(|l| l.index),
                storage_element_index: ls.map(|l| l.storage_element_index).flatten(),
            };
            debug!("ts={ts:#?}");
            csv_writer.serialize(ts)?;
        }
        Ok(())
    }
}

fn run(options: &mut Options) -> Result<()> {

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
        Err(e) => Err(eyre!("Could not find mkltfs program: {mkltfs}: {e}", mkltfs=options.mkltfs.display()))?,
    };
    options.fusermount = match which(&options.fusermount) {
        Ok(path) => path,
        Err(e) => Err(eyre!("Could not find fusermount program: {fusermount}: {e}", fusermount=options.fusermount.display()))?,
    };

    if ! options.drive.exists() {
        Err(eyre!("Could not find drive device: {drive}", drive=options.drive.display()))?
    }
    if ! options.loader.exists() {
        Err(eyre!("Could not find loader device: {loader}", loader=options.loader.display()))?
    }
    if ! options.ltfs_path.is_dir() {
        Err(eyre!("Could not find LTFS mount directory:: {ltfs_path}", ltfs_path=options.ltfs_path.display()))?
    }

    match &options.cmd {
        Some(Command::Backup(_)) => options.backup_files()?,
        Some(Command::Restore(_)) => options.restore_files()?,
        Some(Command::ListBackups(_)) => options.list_backups()?,
        Some(Command::ListFiles(_)) => options.list_files()?,
        Some(Command::ListTapes) => options.list_tapes()?,
        Some(Command::FormatLTFS(_)) => info!("{:#?}", options.format_ltfs()?),
        Some(Command::IsMounted) => info!("{:#?}", options.is_mounted()?),
        Some(Command::IsTapeFull) => info!("{:#?}", options.is_tape_full()?),
        Some(Command::LoaderStatus) => options.loader_status_view()?,
        Some(Command::LoadTape(load_tape_cmd)) => info!("{:#?}", options.load_tape(load_tape_cmd.slot)?),
        Some(Command::MountTape) => info!("{:#?}", options.mount_tape()?),
        Some(Command::ReadyStorage) => info!("{:#?}", options.ready_storage()?),
        Some(Command::TapeStatus) => {
            options.init_db()?;
            info!("{:#?}", options.tape_status()?);
        },
        Some(Command::UnmountTape) => info!("{:#?}", options.unmount_tape()?),
        Some(Command::WaitForDrive) => info!("{:#?}", options.wait_for_drive()?),
        Some(Command::WaitForLTFSProcessToTerminate) => info!("{:#?}", options.wait_for_ltfs_process_to_terminate()?),
        _ => {
            Options::clap().print_long_help()?;
            println!("");
            println!("");
            std::process::exit(1);
        }
    };

    Ok(())
}

fn main() -> Result<()> {
    std::env::set_var("RUST_BACKTRACE", "full");
    color_eyre::install()?;

    let mut options = Options::from_args();

    let logspec = if options.trace {"TRACE"} else if options.verbose {"DEBUG"} else {"INFO"};
    debug!("Parsed options: {options:#?}");

    let _logger = if let Some(log) = &options.log {
        Logger::try_with_env_or_str(logspec)?.
            log_to_file(FileSpec::try_from(log)?).
            format_for_files(flexi_logger::detailed_format).
            rotate(Criterion::Size(1_000_000_000),
              Naming::Timestamps,
              Cleanup::KeepCompressedFiles(100),
            ).
            duplicate_to_stderr(if options.trace {Duplicate::Trace} else if options.verbose {Duplicate::Debug} else {Duplicate::Info}).
            format_for_stderr(flexi_logger::colored_detailed_format).
            start()?
    }
    else {
        Logger::try_with_env_or_str(logspec)?.format(flexi_logger::colored_detailed_format).start()?
    };

    log_panics::init();

    if let Err(err) = run(&mut options) {
        error!("{err:?}");
        Err(err)?;
    }
    Ok(())
}

