#[macro_use] extern crate clap;
#[macro_use] extern crate error_chain;

extern crate byteorder;

use byteorder::{ReadBytesExt, BigEndian};

use std::fs::File;
use std::io;
use std::io::{Seek, SeekFrom, Read, Write};


error_chain!{
    foreign_links {
        Io(::std::io::Error);
    }
}


#[derive(Debug)]
enum CompressionType {
    ZLIB,
    LZMA
}


#[derive(Debug)]
enum ArchiveFlags {
    RelativePaths,
    IgnoreCase,
    AbsolutePaths
}


#[derive(Debug)]
struct FileEntry {
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
    fn open(file: &mut File) -> Result<Self> {
        let magic = file.read_u32::<BigEndian>()?;
        if magic != 0x50534152 {
            return Err(Error::from("Invalid magic"));
        }

        let version_major = file.read_u16::<BigEndian>()?;
        let version_minor = file.read_u16::<BigEndian>()?;

        let compression_type = match file.read_u32::<BigEndian>() {
            Ok(value) => {
                match value {
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
            4294967296 => BlockSizeType::U32,
            i => {
                return Err(Error::from(format!("Invalid block size type {}", i)))
            }
        };
        let archive_flags = match file.read_u32::<BigEndian>() {
            Ok(value) => {
                match value {
                    0 => ArchiveFlags::RelativePaths,
                    1 => ArchiveFlags::IgnoreCase,
                    2 => ArchiveFlags::AbsolutePaths,
                    _ => {
                        return Err(Error::from(format!("Invalid archive flags {}", value)));
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
            entries.push(FileEntry { name_digest, index_list_size, length, offset });
        }

        let current_pos = file.seek(SeekFrom::Current(0))?;
        let num_blocks: u64 = (toc_length as u64 - current_pos) / block_size.get_bytecount() as u64;

        let mut block_sizes: Vec<u64> = Vec::new();
        for _ in 0..num_blocks {
            block_sizes.push(file.read_uint::<BigEndian>(block_size.get_bytecount())?);
        }

        Ok(Self {
            version_minor, version_major, compression_type, 
            toc_length, toc_entry_size, toc_entry_count,
            block_size, archive_flags, entries, block_sizes
        })
    }

    fn print_filelist(&self) {
        for i in self.entries.iter() {
            println!("{:?}", i);
        }
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

    fn print_file(&self, file: &mut File, index: usize) -> Result<()> {
        let entry_details = &self.entries[index];
        eprintln!("{:?}", entry_details);

        let current_index: usize = entry_details.index_list_size as usize;
        let blockdetail = self.block_sizes[current_index];
        eprintln!("Block detail stat: {}", blockdetail);

        file.seek(SeekFrom::Start(entry_details.offset))?;
        let mut stdout = io::stdout();

        if blockdetail == entry_details.length {
            eprintln!("Compressed length is the same as original length.");
            let mut datastream = file.take(entry_details.length);
            io::copy(&mut datastream, &mut stdout)?;
        } else {
            let header = file.read_u16::<BigEndian>()?;
            eprintln!("Header value: {:X}", header);
            file.seek(SeekFrom::Start(entry_details.offset))?;
            let mut datastream = file.take(blockdetail);
            io::copy(&mut datastream, &mut stdout)?;
        }

        stdout.flush()?;
        Ok(())
    }
}


fn main() {
    let matches = clap_app!(myapp => 
        (version: "0.1")
        (about: "Extracts PSARC files")
        (@arg file: +required "The file to extract")
    ).get_matches();

    let filename = matches.value_of("file").unwrap();
    let mut file_obj = match File::open(filename) {
        Ok(file) => file,
        Err(e) => panic!(e)
    };
    let psarc = match PSArc::open(&mut file_obj) {
        Ok(psarc) => psarc,
        Err(e) => panic!("{:?}", e)
    };
    psarc.print_details();
    //psarc.print_filelist();
    psarc.print_file(&mut file_obj, 3).unwrap();
}
