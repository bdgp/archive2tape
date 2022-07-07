#![cfg_attr(feature = "cargo-clippy", allow(cyclomatic_complexity, trivial_regex))]
use std::path::{PathBuf, Path};
use std::io::BufReader;
use std::io::BufRead;
use std::collections::HashSet;

use regex::Regex;
use glob::Pattern;

use structopt::StructOpt;
use std::vec::Vec;
use duct::cmd;

use lazy_static::lazy_static;
use lazy_regex::{regex, regex_captures, regex_is_match};

use shell_words;
use which::which;

use color_eyre::eyre::{Result,eyre};
use rusqlite::{params, Connection};
use sysinfo::{ProcessExt, System, SystemExt, Disk, DiskExt};

// /dev/sg0 lto5 drive
// /dev/sg1 lto5 loader
// /dev/sg2 lto8 drive
// /dev/sg4 lto8 loader

#[derive(StructOpt, Debug)]
#[structopt(name = "archive2tape", about = "Incremental tape archiving tool")]
struct Options {
    #[structopt(long = "drive", help = "Path to tape drive device", name="DEVICE", default_value="/dev/tape/by-id/scsi-35000e111c89b5001-nst")]
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
    #[structopt(long = "db", help = "Path to SQLite archive metadata database", name="PATH")]
    db: PathBuf,
    #[structopt(long = "tapes-db", help = "Path to SQLite tapes metadata database", name="PATH")]
    tapes_db: Option<PathBuf>,
    #[structopt(long = "create-db", help = "Create the metadata database if it doesn't exist")]
    create_db: bool,
    #[structopt(long = "ltfs-path", help = "Mount path for LTFS filesystem", name="PATH", default_value="/media/lto8")]
    ltfs_path: PathBuf,
    #[structopt(long = "restore-path", help = "Restore path", name="PATH", default_value=".")]
    restore_path: PathBuf,
    #[structopt(long = "restore", help = "Perform a restore instead of an archive")]
    restore: bool,
    #[structopt(long = "tape", help = "List of tapes to use for backup and restore. If none specified, use any tapes", name="LABEL")]
    tapes: Vec<String>,
    #[structopt(long = "include", help = "Regex patterns to include files", name="REGEX")]
    include: Vec<Regex>,
    #[structopt(long = "exclude", help = "Regex patterns to exclude files", name="REGEX")]
    exclude: Vec<Regex>,
    #[structopt(long = "ignore-tape", help = "Regex patterns to ignore tapes with matching label", name="REGEX", default_value="^CLN")]
    ignore_tape: Regex,
    #[structopt(help = "Glob patterns to match files in filesystem", name="PATTERN")]
    patterns: Vec<Pattern>,
}

#[derive(PartialEq, Eq, Clone)]
enum StorageType {
    Drive,
    Element,
    ImportExport,
}

#[derive(PartialEq, Eq, Clone)]
enum StorageStatus {
    Empty,
    Full,
}

#[derive(PartialEq, Eq, Clone)]
struct StorageElement {
    index: usize,
    label: Option<String>,
    storage_type: StorageType,
    status: StorageStatus,
    storage_element_index: Option<usize>,
}

#[derive(PartialEq, Eq, Clone)]
struct Tape {
    id: i64,
    label: String,
    is_full: bool,
    date_added: String,
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

    fn is_writeable(&self) -> Result<bool> {
        let mut system = sysinfo::System::new();
        system.refresh_disks_list();
        for disk in system.disks_mut() {
            if disk.mount_point() == self.ltfs_path {
                return Ok(disk.available_space() > 0);
            }
        }
        Err(eyre!("No disk matching {ltfs_path} was found!", ltfs_path=self.ltfs_path.display()))
    }

    fn tape_status(&self) -> Result<Vec<Tape>> {
        let mut tapes = Vec::<Tape>::new();
        if let Some(tapes_db) = &self.tapes_db {
            let conn = Connection::open(tapes_db)?;
            conn.execute(r#"
            create table if not exists tape (
                id integer primary key autoincrement not null,
                label text not null unique,
                is_full integer not null,
                date_added timestamp default current_timestamp not null
            )
            "#, [])?;

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

    fn loaded_tape(&self) -> Result<Option<StorageElement>> {
        let ls = self.loader_status()?;
        let loaded_tape = ls.iter().map(|se| 
            if se.storage_type==StorageType::Drive && se.status==StorageStatus::Full
                { Some((*se).clone()) }
                else { None }
            ).next().unwrap_or(None);
        Ok(loaded_tape)
    }

    fn wait_for_drive(&self) -> Result<()> {
        let mut drive_is_ready = false;
        for i in 0..300 {
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

    fn load_tape(&self, slot: usize) -> Result<()> {
        let ls = self.loader_status()?;
        let drive_slot = ls.iter().
            filter(|se| se.storage_type==StorageType::Drive).
            next().ok_or(eyre!("Could not find drive slot"))?;

        if drive_slot.status == StorageStatus::Full {
            let empty_slot = ls.iter().
                filter(|se| se.storage_type==StorageType::Drive && se.status==StorageStatus::Empty).
                next().ok_or(eyre!("Could not find an empty slot"))?;
            
            cmd!(&self.mtx, "-f", &self.loader, "unload", empty_slot.index.to_string(), drive_slot.index.to_string()).run()?;
        }
        cmd!(&self.mtx, "-f", &self.loader, "load", slot.to_string(), drive_slot.index.to_string()).run()?;
        self.wait_for_drive()?;
        Ok(())
    }

    fn mount_tape(&self) -> Result<()> {
        let cmd = cmd!(&self.ltfs, "-o", format!("devname={drive}", drive=self.drive.to_string_lossy()), &self.ltfs_path);
        let result = cmd.run()?;
        if ! result.status.success() {
            Err(eyre!("ltfs command {cmd:?} failed with status {status:?}", status=result.status.code()))?
        }
        if ! self.is_mounted()? {
            Err(eyre!("path {ltfs_path} was not mounted after running ltfs command {cmd:?}", ltfs_path=self.ltfs_path.display()))?
        }
        Ok(())
    }

        // mtx -f /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 load $slot 0
        // sleep ...

        // loop: mt -f /dev/tape/by-id/scsi-35000e111c89b5001-nst status |grep ONLINE

    fn ready_storage(&self) -> Result<()> {
        // get loader status
        let ls = self.loader_status()?;
        let lt = self.loaded_tape()?;
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
            filter(|se| se.storage_type == StorageType::Element && se.status==StorageStatus::Full).
            collect::<Vec<_>>();
        tapes.sort_by_key(|t| 
            (!(if let Some(label) = &t.label {non_full_tapes.contains(label)} else {false}), 
            &t.label));

        // if not tape is loaded in the drive
        if lt.is_none() {
            // find a tape to load
            for tape in &tapes {
                if tape.storage_type == StorageType::Element {
                    if let Some(label) = &tape.label {
                        // if the tape is not marked as full
                        if ! full_tapes.contains(label) && ! self.ignore_tape.is_match(label) {
                            // load the tape into the drive
                            self.load_tape(tape.index)?;
                            // attempt to mount the tape
                            self.mount_tape()?;
                            break
                        }
                    }
                }
            }
        }

        // ensure drive is writeable, otherwise get another tape
        // unmount if mounted
        // eject tape from drive if tape in drive
        // wait for ltfs process to terminate
        // move tape into drive
        // mount ltfs volume

        Ok(())
    }
}

fn main() -> Result<()> {
    std::env::set_var("RUST_BACKTRACE", "full");
    std::env::set_var("RUST_LIB_BACKTRACE", "1");

    // /usr/bin/mtx - mtx package
    // /usr/bin/mt - cpio
    // /usr/local/bin/ltfs, mkltfs - from ltfs package

    // loader = '/dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0'
    // drive = '/dev/tape/by-id/scsi-35000e111c89b5001-nst'

    // usage: mtx-changer ctl-device command [slot archive-device drive-index]

    // /usr/lib/bareos/scripts/mtx-changer /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 listall 0 /dev/tape/by-id/scsi-35000e111c89b5001-nst 0

    // /usr/lib/bareos/scripts/mtx-changer /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 listall

    // mtx -f /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 status

    // perl -ne '
    // Data Transfer Element (\d+):Empty/ && print "D:$1:E\n";
    // Data Transfer Element (\d+):Full \(Storage Element (\d+) Loaded\)(:VolumeTag =\s*(.+))?/ && print "D:$1:F:$2:$4\n";
    // Storage Element (\d+):Empty/ && print "S:$1:E\n";
    // Storage Element (\d+):Full( :VolumeTag=(.+))?/ && print "S:$1:F:$3\n";
    // Storage Element (\d+) IMPORT.EXPORT:Empty/ && print "I:$1:E\n";
    // Storage Element (\d+) IMPORT.EXPORT:Full( :VolumeTag=(.+))?/ && print "I:$1:F:$3\n";' ${TMPFILE}

    // /usr/lib/bareos/scripts/mtx-changer /dev/sg0 unload 3 /dev/nst0 0

    // umount /media/lto8
    // wait for ltfs process to terminate
    // mt -f /dev/tape/by-id/scsi-35000e111c89b5001-nst offline
    // sleep ...
    // mtx -f /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 unload $slot 0

    // /usr/lib/bareos/scripts/mtx-changer /dev/sg0 load 3 /dev/nst0 0

    // mtx -f /dev/tape/by-id/scsi-1BDT_FlexStor_II_00DE64105985_LL0 load $slot 0
    // sleep ...

    // loop: mt -f /dev/tape/by-id/scsi-35000e111c89b5001-nst status |grep ONLINE


    // scan source directory for files not found in sqlite table
    // check host, path, size, timestamp, and optionally md5sum

    // find the first non-full tape
    // unload any tapes from drive
    // umount /media/lto8 if mounted
    // wait for ltfs process to terminate
    // load the tape
    // format it if it's unformatted
    // write the tape db record
    // mount the tape
    // transfer the files

    // if tape becomes full (copying files to tape fails), 
    // umount /media/lto8
    // wait for ltfs process to terminate  
    // re-mount /media/lto8
    // write the file db records, optionally md5sum
    // then start over

    // write the file db records, optionally md5sum
    // transfer sqlite database to nfs storage for backup
    // run rotate-backups script

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
    if let Some(tapes_db) = options.tapes_db {
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

    Ok(())
}
