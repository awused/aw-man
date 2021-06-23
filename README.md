# AW-MAN

Awused's personal manga reader/image viewer.

<!-- It is a simple viewer with support for running arbitrary upscalers, like waifu2x, and almost no customization. -->

<!-- TODO see if windows support is easy enough -->

# Features

* Fast and memory efficient reader. Persistent memory usage even with 4K images can be only a few hundred MB, though heap usage can spike higher before garbage collection.
* Support for zip, rar, and 7zip archives.
* Proper natural sorting of chapters even with decimal chapter numbers. Works well with [manga-syncer](https://github.com/awused/manga-syncer).
* Not much more, anything I don't personally use doesn't get implemented.
<!-- * Configurable shortcuts to run external scripts. -->
<!-- * Support for custom external upscalers. See [aw-upscale](https://github.com/awused/aw-upscale). -->

# Installation

`go get -u github.com/awused/aw-man`

Copy [aw-man.toml.sample](aw-man.toml.sample) to `~/.config/aw-man/aw-man.toml` or `~/.aw-man.toml` and fill it out according to the instructions.

If you have trouble getting upscaling to work, make sure that waifu2x-ncnn-vulkan is on your PATH. The directory containing the waifu2x-ncnn-vulkan binary should also contain the [models-cunet](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models/models-cunet) directory.

Additional optional files for installation can be found in the [desktop](desktop) directory.

# Dependencies

Required:

* Development libraries for gio your platform - See [gio](https://gioui.org/doc/install) for installation instructions.

Optional:


* [libvips](https://github.com/libvips/libvips#install) is used to provide support for more formats than Go natively supports.
    * If lipvips and its development headers are not available, build with the `novips` tag: `go get -u -tags novips github.com/awused/aw-man`.
* 7z - Support for 7z archives is provided by the 7z binary. The native Go implementations were not performant.
    * If the 7z binary is not present, 7z archives will fail to open.
* GDK - GDK is used to provide support for even more image formats, if available and configured.
    * On Fedora the package is `gtk3-devel`.
    * I am not confident enough in its implementation to enable it by default. The existing Go bindings did not work and even after modification it may be unstable or leaky.
    * The integration will probably be rewritten from scratch in pure C.
    * It must be enabled with the `gdk` build tag: `go get -u -tags gdk github.com/awused/aw-man`.

<!-- * [ImageMagick 6 or 7](https://imagemagick.org/script/download.php) Is used by the default upscaler.
* [waifu2x-ncnn-vulkan](https://github.com/nihui/waifu2x-ncnn-vulkan) When installing waifu2x, make sure that the [models](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models) directory is present (copied or symlinked) in the same directory as the executable.

Alternative upscalers can be configured in place of waifu2x-ncnn-vulkan, see [aw-upscale](https://github.com/awused/aw-upscale). -->

# Usage

Run `aw-man archive-of-images.zip` and view the images. Also works non-recursively on directories of images. Push `U` to switch to viewing an upscaled version of the images.

<!-- The manga mode (`-manga`, `-m` or the `M` shortcut) causes it to treat the directory containing the archive as it if contains a series of volumes or chapters of manga. The next chapter or volume should follow after the last page of the current archive. Supports the directory structure produced by [manga-syncer](https://github.com/awused/manga-syncer) but should work with any archives that sort sensibly. -->

# Shortcuts

Shortcut | Action
---------| ----------
`Up Arrow/Page Up/Mouse Wheel Up` | Moves to the previous page.
`Down Arrow/Page Down/Mouse Wheel Down` | Moves to the next page.
`]` | Moves to the next archive in the same directory.
`[` | Moves to the previous archive in the same direcotry.
`Home/End` | Moves to the First/Last page in the current archive.
`Q/Esc` | Quit.
`H` | Hide the UI.
<!-- `U` | Toggle upscaling with waifu2x. -->
<!-- `M` | Toggle manga mode, enabling continuous scrolling through chapters in the same directory. -->
<!-- `Shift+U` | Toggle upscaling in the background even when viewing normal sized images. -->
<!-- `J  + number + Enter` | Jump to the specified image. -->

# Custom Shortcuts

Custom shortcuts can be defined in [aw-man.toml](aw-man.toml.sample). See the comments in the config file for how to specify them. Each shortcut must be an executable which will be called with several environment variables set.

Environment Variable | Explanation
-------------------- | ----------
AWMAN_ARCHIVE | The path to the current archive or directory that is open.
AWMAN_ARCHIVE_TYPE | The type of the archive, valid values are zip, rar, 7z, dir, or unknown.
AWMAN_RELATIVE_FILE_PATH | The path of the current file relative to the root of the archive or directory.
AWMAN_PAGE_NUMBER | The page number of the currently open file.
AWMAN_CURRENT_FILE | The path to the extracted file or, in the case of directories, the original file. It should not be modified or deleted.

# Scripting

If configured, aw-man will expose a limited API over a unix socket, one per process. See the documentation in [aw-man.toml](aw-man.toml.sample) and the [example script](examples/socket-print.sh).

Request | Response
--------|---------------------------------------------------------------------------------------
status  | The same set of environment variables sent to shortcut executables.
<!-- TODO -- implement the rest of the GUI actions as API calls -->

# Why

I wrote [manga-upscaler](https://github.com/awused/manga-upscaler) for use with mangadex's web viewer but now have a need for something more controllable. Most of the complexity of an image viewer or comic book reader comes from all the customization offered and aw-man has none of that. This program is very much written to fit my needs and little more.
