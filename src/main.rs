#[macro_use] extern crate clap;
#[macro_use] extern crate error_chain;

extern crate byteorder;
extern crate flate2;
extern crate fuse;
extern crate id_tree;
extern crate libc;
extern crate lzma_rs;
extern crate math;

use byteorder::{ReadBytesExt, BigEndian};
use flate2::bufread::ZlibDecoder;
use id_tree::InsertBehavior::{AsRoot, UnderNode};
use id_tree::{Node, NodeId, Tree, TreeBuilder};
use fuse::{FileType, FileAttr, Filesystem, Request, ReplyData, ReplyEntry, ReplyAttr, ReplyDirectory};
use libc::ENOENT;
use lzma_rs::lzma_decompress;
use math::round;

use std::cmp::min;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::io::{Cursor, Seek, SeekFrom, Read, BufReader};
use std::time::{Duration, UNIX_EPOCH};


error_chain!{
    foreign_links {
        Io(::std::io::Error);
    }
}


#[derive(Debug, Copy, Clone)]
enum CompressionType {
    None,
    ZLIB,
    LZMA
}


#[derive(Debug)]
enum ArchiveFlags {
    RelativePaths,
    IgnoreCase,
    AbsolutePaths,
    Unknown
}


#[derive(Debug)]
struct FileEntry {
    name: String,
    name_digest: [u8; 16],
    index_list_size: u32,
    length: u64,
    offset: u64
}

#[derive(Debug)]
enum BlockSizeType {
    U16,
    U24,
    U32
}

impl BlockSizeType {
    fn get_bytecount(&self) -> usize {
        match self {
            BlockSizeType::U16 => 2,
            BlockSizeType::U24 => 3,
            BlockSizeType::U32 => 4
        }
    }

    fn get_bitcount(&self) -> u64 {
        match self {
            BlockSizeType::U16 => 65536,
            BlockSizeType::U24 => 16777216,
            BlockSizeType::U32 => 4294967296
        }
    }
}


#[derive(Debug)]
struct PSArc {
    version_minor: u16,
    version_major: u16,
    compression_type: CompressionType,
    toc_length: u32,
    toc_entry_size: u32,
    toc_entry_count: u32,
    block_size: BlockSizeType,
    archive_flags: ArchiveFlags,
    entries: Vec<FileEntry>,
    block_sizes: Vec<u64>
}

impl PSArc {
    fn open(file: &mut BufReader<File>) -> Result<Self> {
        let empty_string = "".to_string();
        let magic = file.read_u32::<BigEndian>()?;
        if magic != 0x50534152 {
            return Err(Error::from("Invalid magic"));
        }

        let version_major = file.read_u16::<BigEndian>()?;
        let version_minor = file.read_u16::<BigEndian>()?;

        let compression_type = match file.read_u32::<BigEndian>() {
            Ok(value) => {
                match value {
                    0x00000000 => CompressionType::None,
                    0x7A6C6962 => CompressionType::ZLIB,
                    0x6C7A6D61 => CompressionType::LZMA,
                    _ => {
                        return Err(Error::from(format!("Invalid compression type {}", value)));
                    }
                }
            },
            Err(e) => { return Err(Error::from(e)); }
        };

        let toc_length = file.read_u32::<BigEndian>()?;
        let toc_entry_size = file.read_u32::<BigEndian>()?;
        let toc_entry_count = file.read_u32::<BigEndian>()?;
        let block_size = match file.read_u32::<BigEndian>()? {
            65536 => BlockSizeType::U16,
            16777216 => BlockSizeType::U24,
            4294967295 => BlockSizeType::U32,
            i => {
                return Err(Error::from(format!("Invalid block size type {}", i)))
            }
        };
        let archive_flags = match file.read_u32::<BigEndian>() {
            // TODO: replace this with bitflags.
            Ok(value) => {
                match value {
                    0 => ArchiveFlags::RelativePaths,
                    1 => ArchiveFlags::IgnoreCase,
                    2 => ArchiveFlags::AbsolutePaths,
                    _ => {
                        println!("Invalid archive flags {}", value);
                        ArchiveFlags::Unknown
                    }
                }
            },
            Err(e) => { return Err(Error::from(e)); }
        };

        let mut entries: Vec<FileEntry> = Vec::new();
        for _ in 0..toc_entry_count {
            let mut name_digest: [u8; 16] = [0; 16];
            for pos in 0..16 {
                name_digest[pos] = file.read_u8::<>()?;
            }
            let index_list_size = file.read_u32::<BigEndian>()?;
            let length = file.read_uint::<BigEndian>(5)?;
            let offset = file.read_uint::<BigEndian>(5)?;
            entries.push(FileEntry { name: empty_string.clone(), name_digest, index_list_size, length, offset });
        }

        let current_pos = file.seek(SeekFrom::Current(0))?;
        let num_blocks: u64 = (toc_length as u64 - current_pos) / block_size.get_bytecount() as u64;

        let mut block_sizes: Vec<u64> = Vec::new();
        for _ in 0..num_blocks {
            block_sizes.push(file.read_uint::<BigEndian>(block_size.get_bytecount())?);
        }


        let mut i = Self {
            version_minor, version_major, compression_type, 
            toc_length, toc_entry_size, toc_entry_count,
            block_size, archive_flags, entries, block_sizes
        };
        i.parse_manifest(file)?;
        return Ok(i);
    }

    fn parse_manifest(&mut self, file: &mut BufReader<File>) -> Result<()> {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        self.print_file(file, &mut cursor, 0, None)?;
        let mut string_data = String::new();
        cursor.seek(SeekFrom::Start(0))?;
        cursor.read_to_string(&mut string_data)?;
        let mut lines: Vec<&str> = string_data.lines().collect();
        let manifest_name = match self.archive_flags {
            ArchiveFlags::AbsolutePaths => "/manifest.txt",
            _ => "manifest.txt"
        };
        lines.insert(0, manifest_name);
        for (i, line) in lines.iter().enumerate() {
            self.entries[i].name = line.to_string();
        }
        Ok(())
    }

    fn print_details(&self) {
        eprintln!("Version:\t\t{}.{}", self.version_major, self.version_minor);
        eprintln!("Compression type:\t{:?}", self.compression_type);
        eprintln!("TOC length:\t\t{}", self.toc_length);
        eprintln!("TOC entry size:\t\t{}", self.toc_entry_size);
        eprintln!("TOC entry count:\t{}", self.toc_entry_count);
        eprintln!("Block size:\t\t{:?}", self.block_size);
        eprintln!("Archive flags:\t\t{:?}", self.archive_flags);
        eprintln!("Amount of blocks registered:\t{}", self.block_sizes.len());
    }

    fn print_file<W: std::io::Seek + io::Write>(&self, file: &mut BufReader<File>, out: &mut W, index: usize, amount: Option<u64>) -> Result<()> {
        let entry_details = &self.entries[index];
        let amount = match amount {
            Some(amt) => amt,
            _ => entry_details.length
        };
        let blocks = round::ceil(entry_details.length as f64 / self.block_size.get_bitcount() as f64, 0) as u64;
        file.seek(SeekFrom::Start(entry_details.offset))?;

        let compression = match file.read_u16::<BigEndian>() {
            Ok(value) => {
                match value {
                    0x78da | 0x7801 => CompressionType::ZLIB,
                    0x5D00 => CompressionType::LZMA,
                    _ => CompressionType::None
                }
            },
            Err(e) => { 
                return Err(Error::from(e)); 
            }
        };
        file.seek(SeekFrom::Start(entry_details.offset))?;
        let mut bytes_written = 0;
        match compression {
            CompressionType::None => {
                let filesize = entry_details.length;
                let mut datastream = file.take(filesize);
                io::copy(&mut datastream, out)?;
            },
            CompressionType::LZMA => {
                for _ in 0..blocks {
                    let mut datastream = file.take(self.block_size.get_bitcount());
                    lzma_decompress(&mut datastream, out).unwrap();
                    let current_pos = out.seek(SeekFrom::Current(0))?;
                    if current_pos > amount {
                        return Ok(());
                    }
                }
            },
            CompressionType::ZLIB => {
                for _ in 0..blocks {
                    let datastream = file.take(self.block_size.get_bitcount());
                    let mut decoder = ZlibDecoder::new(datastream);
                    bytes_written += io::copy(&mut decoder, out)?;
                    eprintln!("Bytes written: {:?} of {:?}", bytes_written, amount);
                    if bytes_written > amount { 
                        return Ok(());
                    }
                }
            },
        }

        Ok(())
    }
}


pub type Inode = u64;

const ROOT_INODE: Inode = 1;
const TTL: Duration = Duration::from_secs(60);           // 1 second


enum InodeData {
    Folder(String),
    ArchivedFile(String, usize)
}


struct PSArcFS {
    psarc: PSArc,
    reader: BufReader<File>,
    tree: Tree<Inode>,
    files: HashMap<Inode, InodeData>,
    node_ids: HashMap<Inode, NodeId>,
    cache: HashMap<Inode, [u8; 16384]>,
}

impl PSArcFS {
    fn new(psarc: PSArc, reader: BufReader<File>) -> Self {
        let mut tree = TreeBuilder::new().with_node_capacity(10000).build();
        let mut files = HashMap::new();
        let mut node_ids = HashMap::new();
        let mut folder_names: HashMap<String, Inode> = HashMap::new();

        let mut inode_counter = ROOT_INODE;

        files.insert(inode_counter, InodeData::Folder(".".to_string()));
        let root_id: NodeId = tree.insert(Node::new(inode_counter), AsRoot).unwrap();
        node_ids.insert(inode_counter, root_id);
        folder_names.insert(".".to_string(), inode_counter);

        inode_counter += 1;

        for (i, entry) in psarc.entries.iter().enumerate() {
            let mut split_path = entry.name.split('/').filter(|x| x.len() > 0).peekable();
            let mut current_path = "".to_string();
            let mut parent_inode = ROOT_INODE;
            while let Some(name) = split_path.next() {
                if split_path.peek().is_some() {
                    current_path.push_str(name);
                    current_path.push('/');
                    match folder_names.get(&current_path) {
                        Some(inode_id) => {
                            parent_inode = inode_id.clone();
                        },
                        None => {
                            let node_id = node_ids.get(&parent_inode).unwrap();
                            files.insert(inode_counter, InodeData::Folder(name.to_string()));
                            let root_id: NodeId = tree.insert(Node::new(inode_counter), UnderNode(node_id)).unwrap();
                            node_ids.insert(inode_counter, root_id);
                            folder_names.insert(current_path.clone(), inode_counter);
                            parent_inode = inode_counter.clone();
                            inode_counter += 1;
                        }
                    }
                } else {
                    let node_id = node_ids.get(&parent_inode).unwrap();
                    files.insert(inode_counter, InodeData::ArchivedFile(name.to_string(), i.clone()));
                    let root_id: NodeId = tree.insert(Node::new(inode_counter), UnderNode(node_id)).unwrap();
                    node_ids.insert(inode_counter, root_id);
                    parent_inode = inode_counter.clone();
                    inode_counter += 1;
                }
            }
        }

        Self {
            psarc: psarc,
            reader: reader,
            tree: tree,
            files: files,
            node_ids: node_ids,
            cache: HashMap::new(),
        }
    }
}


impl Filesystem for PSArcFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.node_ids.get(&parent) {
            Some(node_obj) => {
                for child in self.tree.children(node_obj).unwrap() {
                    let inode = child.data().clone();
                    match self.files.get(&inode) {
                        Some(InodeData::Folder(f)) => {
                            if name.to_str().unwrap() == f {
                                let attrs = FileAttr {
                                    ino: inode,
                                    size: 0,
                                    blocks: 0,
                                    atime: UNIX_EPOCH,                                  // 1970-01-01 00:00:00
                                    mtime: UNIX_EPOCH,
                                    ctime: UNIX_EPOCH,
                                    ftype: FileType::Directory,
                                    perm: 0o755,
                                    nlink: 2,
                                    uid: 0,
                                    gid: 0,
                                    rdev: 0,
                                };
                                reply.entry(&TTL, &attrs, 0);
                                return;
                            }
                        },
                        Some(InodeData::ArchivedFile(f, index)) => {
                            if name.to_str().unwrap() == f {
                                let attrs = FileAttr {
                                    ino: inode,
                                    size: self.psarc.entries.get(index.clone()).unwrap().length,
                                    blocks: 0,
                                    atime: UNIX_EPOCH,                                  // 1970-01-01 00:00:00
                                    mtime: UNIX_EPOCH,
                                    ctime: UNIX_EPOCH,
                                    ftype: FileType::RegularFile,
                                    perm: 0o755,
                                    nlink: 2,
                                    uid: 0,
                                    gid: 0,
                                    rdev: 0,
                                };
                                reply.entry(&TTL, &attrs, 0);
                                return;
                            }
                        },
                        None => {},
                    }
                }
            },
            _ => {}
        }

        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match self.files.get(&ino) {
            Some(InodeData::Folder(_)) => {
                let attrs = FileAttr {
                    ino: ino,
                    size: 0,
                    blocks: 0,
                    atime: UNIX_EPOCH,                                  // 1970-01-01 00:00:00
                    mtime: UNIX_EPOCH,
                    ctime: UNIX_EPOCH,
                    ftype: FileType::Directory,
                    perm: 0o755,
                    nlink: 2,
                    uid: 0,
                    gid: 0,
                    rdev: 0,
                };
                reply.attr(&TTL, &attrs);
            },
            Some(InodeData::ArchivedFile(_, id)) => {
                let id = id.clone();
                let file = &self.psarc.entries[id];
                let attrs = FileAttr {
                    ino: ino,
                    size: file.length,
                    blocks: 0,
                    atime: UNIX_EPOCH,                                  // 1970-01-01 00:00:00
                    mtime: UNIX_EPOCH,
                    ctime: UNIX_EPOCH,
                    ftype: FileType::RegularFile,
                    perm: 0o644,
                    nlink: 1,
                    uid: 0,
                    gid: 0,
                    rdev: 0,
                };
                reply.attr(&TTL, &attrs);
            },
            _ => reply.error(ENOENT),
        }
    }

    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, size: u32, reply: ReplyData) {
        print!("read called for inode {:?}, offset {:?}, size {:?}", ino, offset, size);
        if offset == 0 {
            if size <= 16384 {
                match self.cache.get(&ino) {
                    Some(cached_data) => {
                        println!(" => served from cache");
                        reply.data(&cached_data[..size as usize]);
                        return;
                    }
                    _ => {}
                }
            }
        }

        let file_index = match self.files.get(&ino) {
            Some(InodeData::ArchivedFile(_, id)) => id,
            _ => {
                reply.error(ENOENT);
                return;
            }
        };

        let mut cursor = Cursor::new(Vec::<u8>::new());
        self.psarc.print_file(&mut self.reader, &mut cursor, file_index.clone(), Some(offset as u64 + size as u64)).unwrap();
        cursor.seek(SeekFrom::Start(0)).unwrap();
        let list_of_bytes = cursor.get_ref();
        let end = min(offset as usize + size as usize, list_of_bytes.len());
        if offset == 0 {
            if list_of_bytes.len() > 16384 {
                let mut cache_arr: [u8; 16384] = [0; 16384];
                cache_arr.copy_from_slice(&list_of_bytes[..16384]);
                self.cache.insert(ino, cache_arr);
            }
        }
        reply.data(&list_of_bytes[offset as usize..end]);
        println!(" => served from archive");
    }

    fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let dir_node = match self.node_ids.get(&ino) {
            Some(node_id) => node_id,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        if offset == 0 {
            reply.add(ino, 1, FileType::Directory, ".");
        }

        match self.tree.ancestors(dir_node).unwrap().next() {
            Some(x) => {
                match self.files.get(x.data()) {
                    Some(InodeData::Folder(_)) => {
                        if offset < 2 {
                            reply.add(x.data().clone(), 2, FileType::Directory, "..");
                        }
                    }
                    _ => {}
                };
            },
            None => {
                if offset < 2 {
                    reply.add(1, 2, FileType::Directory, "..");
                }
            }
        };
        for (i, child) in self.tree.children(dir_node).unwrap().enumerate().skip(offset as usize) {
            let inode = child.data().clone();
            match self.files.get(&inode) {
                Some(InodeData::Folder(f)) => {
                    reply.add(inode, (i + 2) as i64, FileType::Directory, f);
                },
                Some(InodeData::ArchivedFile(f, _)) => {
                    reply.add(inode, (i + 2) as i64, FileType::RegularFile, f);
                },
                None => {},
            }
        }

        reply.ok();
    }
}


fn main() {
    let matches = clap_app!(myapp => 
        (version: "0.1")
        (about: "Extracts PSARC files")
        (@arg file: +required "The file to extract")
        (@arg mountpoint: "Place to mount archive via FUSE")
    ).get_matches();

    let filename = matches.value_of("file").unwrap();
    let file_obj = match File::open(filename) {
        Ok(file) => file,
        Err(e) => panic!(e)
    };
    let mut reader = BufReader::new(file_obj);
    let psarc = match PSArc::open(&mut reader) {
        Ok(psarc) => psarc,
        Err(e) => panic!("{:?}", e)
    };
    psarc.print_details();
    
    match matches.value_of("mountpoint") {
        Some(mountpoint) => {
            let psarcfs = PSArcFS::new(psarc, reader);
            let fsname = format!("fsname={}", filename);
            let raw_options = ["-o", "ro", "-o", &fsname, "-o", "auto_unmount", "-o", "subtype=psarc", "-o", "auto_cache"];
            let options = raw_options.iter().map(|o| o.as_ref()).collect::<Vec<&OsStr>>();

            match fuse::mount(psarcfs, &mountpoint.to_string(), &options) {
                Ok(_) => { println!("all ok!"); },
                Err(e) => { println!("{:?}", e); }
            }

        },
        _ => {},
    };
}
