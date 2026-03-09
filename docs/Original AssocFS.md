#assoc 

# Associative Persistent FS

> [Prefer associative ontologies to hierarchical taxonomies](https://notes.andymatuschak.org/z29hLZHiVt7W2uss2uMpSZquAX5T6vaeSF6Cy) -- Andy Matuschak 

## Required properties
 
 * support for uninterpreted data storages (aka files)
 * support for multiple interpretations of files
 * support for optional database alike properties to make data and metadata indexing simpler (@sa beagle/kerry) and application-data storage easier (@sa akonadi) (@sa BFS metadata)
 * support seamless volumes across a number of hardware devices. (@sa Btrfs/Bcachefs)
 * _non-hierarchical_ (associative) fs
 * support filesystem quotas
 * endianness-adaptive
 * journalled/atomic
 * proper acls - list of users and groups that can access files (@sa NTFS)

## Nice-to-have properties

 * versioning/snapshotting support (@sa Btrfs) - stores only modified blocks plus updated metadata.
 * sort of file change logs (necessary for servers, nice to haves for desktop - e.g. for backup software? - also eliminates need for atime, @sa relatime)
 * VxFS-like - upgradeable on-disk format while mounted.

 - [ ] OS block cache (see-also::[[BcacheFS]])
   - [ ] TESTS
 - [ ] write b+tree handler
   - [ ] TESTS
 - [ ] Create architecture descriptions in increasing level of detail (@sa ANSA engineering overview).

#### Similar filesystems

 * [Btrfs](http://btrfs.wiki.kernel.org/index.php/Main_Page)+BeFS might be a good starting point.
 * ZFS is too complicated.
 * BcacheFS?

Pools and their associated ZFS file systems can be moved between different platform architectures, including systems implementing different byte orders. The ZFS block pointer format stores filesystem metadata in an endian-adaptive way; individual metadata blocks are written with the native byte order of the system writing the block. When reading, if the stored endianness doesn't match the endianness of the system, the metadata is byte-swapped in memory.

### Design

- [Encryption](https://en.wikipedia.org/wiki/Disk_encryption_theory) (LRW, XTS, HCTR2, HBSH)
- Metadata database
- Content-addressable-storage
- Snapshotting
- Journaling

### Details

#### Directory index

Single string table with filenames.
strscan in table yields offset, which is used for inode lookup (as a hash).

! not filenames, but tags and attribute=value pairs for associative fs.
attributes include system-assigned mime-type and source-url tags (for type and saved-from url respectively)

#### Notes

http://en.wikipedia.org/wiki/Log-structured_file_system

https://en.wikipedia.org/wiki/Semantic_file_system

There's something being developed at PARC that pretty much looks like Mettā's assocfs: [Content-Centric Network](http://www.xconomy.com/san-francisco/2012/08/07/the-next-internet-inside-parcs-vision-of-content-centric-networking/?single_page=true). (**DEAD LINK**)

----

Attributes read through open handle of file. Special open flag ATTR_{READ,WRITE,etc} for dealing with attrs only.

 * Hey, no silly posixish interfaces please, use ifileattr interface or sth like that.

----

TRACE flag - set when file is a trace, that is, no actual data is associated with it's name but only metadata and accounting information. Used for versioned files which are no longer available on disk because of snapshot removal process (to free up memory). Files may include a backup-url property to indicate where data may be found (e.g. if it was archived).

----

#### How to make assocfs navigable for users

 * through helper components that make manipulating and querying relevant attributes easy

Example: software project. Source files are tagged with PROJECT(_RELATION)=myproj,PROJECT_FILETYPE=source.
Adding file to project is as easy as adding these two attribs, however user should be free from doing this manually.
Need to scope attribute namespaces, dev/project and business/project are not quite the same types of projects - a non-hierarchical namespaces on attributes themselves? Seems to be getting complicated.

A naming context style attributes, serialized to disk? Needs efficient lookup in huge context graphs...

---

https://bcachefs.org/Architecture/
