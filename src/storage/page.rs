use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use super::file_header::PAGE_SIZE;

/// Page types used in the database
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    /// Free/unused page
    Free = 0x00,
    /// Internal B+ tree node
    Internal = 0x01,
    /// Leaf B+ tree node
    Leaf = 0x02,
    /// Overflow page for large values
    Overflow = 0x03,
}

impl PageType {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(PageType::Free),
            0x01 => Some(PageType::Internal),
            0x02 => Some(PageType::Leaf),
            0x03 => Some(PageType::Overflow),
            _ => None,
        }
    }
}

/// A fixed-size page buffer
#[derive(Debug, Clone)]
pub struct Page {
    pub page_num: u64,
    pub data: Vec<u8>,
    pub dirty: bool,
}

impl Page {
    /// Create a new empty page
    pub fn new(page_num: u64) -> Self {
        Self {
            page_num,
            data: vec![0u8; PAGE_SIZE],
            dirty: false,
        }
    }

    /// Create a page from existing data
    pub fn from_data(page_num: u64, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), PAGE_SIZE);
        Self {
            page_num,
            data,
            dirty: false,
        }
    }

    /// Get the page type
    pub fn page_type(&self) -> Option<PageType> {
        PageType::from_byte(self.data[0])
    }

    /// Set the page type
    pub fn set_page_type(&mut self, pt: PageType) {
        self.data[0] = pt as u8;
        self.dirty = true;
    }

    /// Mark the page as dirty (needs to be written to disk)
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Get the raw data
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Get mutable access to raw data (marks as dirty)
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        self.dirty = true;
        &mut self.data
    }
}

/// Manages reading and writing pages to/from a file
pub struct PageManager {
    file: File,
    total_pages: u64,
}

impl PageManager {
    /// Create a new PageManager for the given file
    pub fn new(file: File, total_pages: u64) -> Self {
        Self { file, total_pages }
    }

    /// Get the total number of pages
    pub fn total_pages(&self) -> u64 {
        self.total_pages
    }

    /// Read a page from disk
    pub fn read_page(&mut self, page_num: u64) -> io::Result<Page> {
        if page_num >= self.total_pages {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("page {} out of range (total: {})", page_num, self.total_pages),
            ));
        }

        let offset = page_num * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;

        let mut data = vec![0u8; PAGE_SIZE];
        self.file.read_exact(&mut data)?;

        Ok(Page::from_data(page_num, data))
    }

    /// Write a page to disk
    pub fn write_page(&mut self, page: &Page) -> io::Result<()> {
        let offset = page.page_num * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&page.data)?;
        Ok(())
    }

    /// Allocate a new page, returns the page number
    pub fn allocate_page(&mut self) -> io::Result<u64> {
        let page_num = self.total_pages;
        self.total_pages += 1;

        // Initialize empty page on disk
        let page = Page::new(page_num);
        self.write_page(&page)?;

        Ok(page_num)
    }

    /// Sync all pending writes to disk
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

/// A simple page cache using LRU eviction
pub struct PageCache {
    pages: std::collections::HashMap<u64, Page>,
    capacity: usize,
    access_order: std::collections::VecDeque<u64>,
}

impl PageCache {
    /// Create a new page cache with the given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            pages: std::collections::HashMap::with_capacity(capacity),
            capacity,
            access_order: std::collections::VecDeque::with_capacity(capacity),
        }
    }

    /// Get a page from the cache
    pub fn get(&mut self, page_num: u64) -> Option<&Page> {
        if self.pages.contains_key(&page_num) {
            // Move to front of access order
            self.access_order.retain(|&p| p != page_num);
            self.access_order.push_front(page_num);
            self.pages.get(&page_num)
        } else {
            None
        }
    }

    /// Get a mutable reference to a page from the cache
    pub fn get_mut(&mut self, page_num: u64) -> Option<&mut Page> {
        if self.pages.contains_key(&page_num) {
            // Move to front of access order
            self.access_order.retain(|&p| p != page_num);
            self.access_order.push_front(page_num);
            self.pages.get_mut(&page_num)
        } else {
            None
        }
    }

    /// Insert a page into the cache, returns evicted page if cache was full
    pub fn insert(&mut self, page: Page) -> Option<Page> {
        let page_num = page.page_num;

        // If already in cache, just update
        if self.pages.contains_key(&page_num) {
            self.access_order.retain(|&p| p != page_num);
            self.access_order.push_front(page_num);
            self.pages.insert(page_num, page);
            return None;
        }

        // Evict if at capacity
        let evicted = if self.pages.len() >= self.capacity {
            if let Some(lru_page_num) = self.access_order.pop_back() {
                self.pages.remove(&lru_page_num)
            } else {
                None
            }
        } else {
            None
        };

        // Insert new page
        self.access_order.push_front(page_num);
        self.pages.insert(page_num, page);

        evicted
    }

    /// Remove a page from the cache
    pub fn remove(&mut self, page_num: u64) -> Option<Page> {
        self.access_order.retain(|&p| p != page_num);
        self.pages.remove(&page_num)
    }

    /// Get all dirty pages
    pub fn dirty_pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.values().filter(|p| p.dirty)
    }

    /// Clear all dirty flags
    pub fn clear_dirty_flags(&mut self) {
        for page in self.pages.values_mut() {
            page.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_type_roundtrip() {
        assert_eq!(PageType::from_byte(0x00), Some(PageType::Free));
        assert_eq!(PageType::from_byte(0x01), Some(PageType::Internal));
        assert_eq!(PageType::from_byte(0x02), Some(PageType::Leaf));
        assert_eq!(PageType::from_byte(0x03), Some(PageType::Overflow));
        assert_eq!(PageType::from_byte(0xFF), None);
    }

    #[test]
    fn test_page_cache_lru() {
        let mut cache = PageCache::new(2);

        cache.insert(Page::new(1));
        cache.insert(Page::new(2));

        // Access page 1 to make it more recent
        cache.get(1);

        // Insert page 3, should evict page 2 (least recently used)
        let evicted = cache.insert(Page::new(3));
        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().page_num, 2);

        // Page 1 should still be in cache
        assert!(cache.get(1).is_some());
        // Page 2 should be gone
        assert!(cache.get(2).is_none());
        // Page 3 should be in cache
        assert!(cache.get(3).is_some());
    }
}
