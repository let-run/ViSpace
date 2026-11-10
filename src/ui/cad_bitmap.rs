//! Bounded GPU texture reuse across pan/zoom and schematic sheet switches.
use eframe::egui;
use std::collections::BTreeMap;

struct Entry {
    texture: egui::TextureHandle,
    bytes: usize,
    used: u64,
}
pub(super) struct BitmapCache {
    entries: BTreeMap<(String, usize, bool), Entry>,
    bytes: usize,
    limit: usize,
    tick: u64,
}
impl Default for BitmapCache {
    fn default() -> Self {
        Self::new(96 * 1024 * 1024)
    }
}
impl BitmapCache {
    fn new(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            bytes: 0,
            limit,
            tick: 0,
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
    pub fn texture(
        &mut self,
        ctx: &egui::Context,
        file: &str,
        index: usize,
        size: [usize; 2],
        rgba: &[u8],
    ) -> egui::TextureHandle {
        self.texture_with_grayscale(ctx,file,index,size,rgba,false)
    }
    pub fn texture_with_grayscale(&mut self,ctx:&egui::Context,file:&str,index:usize,size:[usize;2],rgba:&[u8],grayscale:bool)->egui::TextureHandle{
        self.tick += 1;
        let key = (file.to_owned(), index, grayscale);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used = self.tick;
            return entry.texture.clone();
        }
        let bytes = rgba.len();
        // A single oversized image remains usable; older images are evicted first.
        while self.bytes.saturating_add(bytes) > self.limit && !self.entries.is_empty() {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .unwrap()
                .0
                .clone();
            self.bytes -= self.entries.remove(&oldest).unwrap().bytes;
        }
        let mut image=egui::ColorImage::from_rgba_unmultiplied(size, rgba);
        if grayscale{for pixel in &mut image.pixels{
            let gray=(pixel.r() as f32*0.2126+pixel.g() as f32*0.7152+pixel.b() as f32*0.0722).round() as u8;
            *pixel=egui::Color32::from_rgba_premultiplied(gray,gray,gray,pixel.a());
        }}
        let texture = ctx.load_texture(
            format!("schematic:{file}:{index}:{grayscale}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.entries.insert(
            key,
            Entry {
                texture: texture.clone(),
                bytes,
                used: self.tick,
            },
        );
        self.bytes += bytes;
        texture
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grayscale_bitmap_is_cached_separately_from_original(){
        let ctx=egui::Context::default();let mut cache=BitmapCache::default();
        let original=cache.texture(&ctx,"sheet",0,[1,1],&[255,0,0,255]).id();
        let gray=cache.texture_with_grayscale(&ctx,"sheet",0,[1,1],&[255,0,0,255],true).id();
        assert_ne!(original,gray);
        assert_eq!(gray,cache.texture_with_grayscale(&ctx,"sheet",0,[1,1],&[255,0,0,255],true).id());
        assert_eq!(original,cache.texture(&ctx,"sheet",0,[1,1],&[255,0,0,255]).id());
        assert_eq!(cache.bytes,8);
    }
    #[test]
    fn reuses_sheet_textures_and_evicts_least_recently_used_and_invalidates_on_reload() {
        let ctx = egui::Context::default();
        let mut cache = BitmapCache::new(32);
        let a = cache.texture(&ctx, "a", 0, [2, 2], &[255; 16]).id();
        let b = cache.texture(&ctx, "b", 0, [2, 2], &[255; 16]).id();
        assert_eq!(a, cache.texture(&ctx, "a", 0, [2, 2], &[255; 16]).id());
        let _ = cache.texture(&ctx, "c", 0, [2, 2], &[255; 16]);
        assert_eq!(cache.bytes, 32);
        assert_eq!(a, cache.texture(&ctx, "a", 0, [2, 2], &[255; 16]).id());
        assert_ne!(b, cache.texture(&ctx, "b", 0, [2, 2], &[255; 16]).id());
        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert_ne!(a, cache.texture(&ctx, "a", 0, [2, 2], &[255; 16]).id());
    }
}
