# AW-Man

Awused's personal manga reader/image viewer.

It is a simple viewer with support for running arbitrary scalers waifu2x upscaling and almost no customization.

<!-- TODO see if windows support is easy enough -->

# Installation

`go get -u github.com/awused/aw-man`

Copy [aw-man.toml.sample](aw-man.toml.sample) to `~/.config/aw-man/aw-man.toml` or `~/.aw-man.toml` and fill it out according to the instructions.

If you have trouble getting upscaling to work, make sure that waifu2x-ncnn-vulkan is on your PATH. The directory containing the waifu2x-ncnn-vulkan binary should also contain the [models](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models) directory (not the models-cunet directory). See [aw-upscale](https://github.com/awused/aw-upscale) if you want to use an alternate upscaler.

# Requirements

* [ImageMagick 6 or 7](https://imagemagick.org/script/download.php)
* Waifu2x
    * [waifu2x-ncnn-vulkan](https://github.com/nihui/waifu2x-ncnn-vulkan) When installing waifu2x, make sure that the [models](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models) directory is present (copied or symlinked) in the same directory as the executable.
* Development libraries for gio your platform - See [gio](https://gioui.org/) docs

Alternative upscalers can be configured, see [aw-upscale](https://github.com/awused/aw-upscale).

# Usage

Run `aw-man archive-of-images.zip` and view the images. Also works non-recursively on directories of images. Push `U` to switch to viewing an upscaled version of the images.

The manga mode (`-manga`, `-m` or the `M` shortcut) causes it to treat the directory containing the archive as it if contains a series of volumes or chapters of manga. The next chapter or volume should follow after the last page of the current archive. Supports the directory structure produced by [manga-syncer](https://github.com/awused/manga-syncer) but should work with any archives that sort sensibly.

# Shortcuts

Shortcut | Action
---------| ----------
`Up Arrow/Page Up/Mouse Wheel Up` | Moves to the previous page.
`Down Arrow/Page Down/Mouse Wheel Down` | Moves to the next page.
`]` | Moves to the next archive in the same directory.
`[` | Moves to the previous archive in the same direcotry.
`Home/End` | Moves to the First/Last page in the current archive.
`U` | Toggle upscaling with waifu2x.
`M` | Toggle manga mode, enabling continuous scrolling through chapters in the same directory.
<!-- `Shift+U` | Toggle upscaling in the background even when viewing normal sized images. -->
`Q/Esc` | Quit.
`H` | Hide the UI.

# Custom Shortcuts

TODO

# Why

I wrote [manga-upscaler](https://github.com/awused/manga-upscaler) for use with mangadex's web viewer but now have a need for something more controllable. Most of the complexity of an image viewer or comic book reader comes from all the customization offered and aw-man has none of that. This program is very much written to fit my needs and little more.