# AW-Manga

Manga/comic reader with support for waifu2x upscaling and almost no customization.

# Usage

`go get -u github.com/awused/aw-manga`

Copy `aw-manga.toml.sample` to `~/.config/aw-manga/aw-manga.toml` or `~/.aw-manga.toml` and fill it out according to the instructions.

Run `aw-manga archive-of-images.zip` and view the images. Also works non-recursively on directories of images.

The `-manga` flag causes it to treat the directory containing the archive as it if contains a series of volumes or chapters of manga. The next chapter or volume should follow after the last page of the current archive. Supports the directory structure produced by [manga-syncer](https://github.com/awused/manga-syncer) but should work with any archives that sort sensibly.

# Requirements

* Waifu2x
    * [waifu2x-ncnn-vulkan](https://github.com/nihui/waifu2x-ncnn-vulkan) Use the cunet model.
* Development libraries for your platform - Se [Gio](https://gioui.org/) docs

# Shortcuts

Shortcut | Action
---------| ----------
`Up Arrow/Page Up/Mouse Wheel Up` | Moves to the previous page.
`Down Arrow/Page Down/Mouse Wheel Down` | Moves to the next page.
`]` | Moves to the next archive in the same directory.
`[` | Moves to the previous archive in the same direcotry.
`Home/End` | Moves to the First/Last page in the current archive.
`U` | Toggle upscaling with waifu2x.
<!-- `Shift+U` | Toggle upscaling in the background even when viewing normal sized images. -->
`Q/Esc` | Quit.
`H` | Hide the UI.

# Custom Shortcuts

TODO

# Why

I wrote [manga-upscaler](https://github.com/awused/manga-upscaler) for use with mangadex's web viewer but now have a need for something more controllable. Most of the complexity of an image viewer or comic book reader comes from all the customization offered and aw-manga has none of that. This program is very much written to fit my needs and little more.
