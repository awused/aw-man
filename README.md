# AW-MAN

Awused's personal manga reader/image viewer.

It is a simple viewer with support for running arbitrary upscalers, like waifu2x.

# Features

* Fast, GPU accelerated, and memory efficient reader.
    * The priorities are latency, then vram usage, then memory usage, then CPU usage.
    * Animated gifs are moderately memory-inefficient.
* Wide support for many archive formats.
* Proper natural sorting of chapters even with decimal chapter numbers.
    * Works well with [manga-syncer](https://github.com/awused/manga-syncer), but generally matches expected sorting order.
* Configurable shortcuts to run external scripts and a basic IPC interface.
* Support for custom external upscalers. See [aw-upscale](https://github.com/awused/aw-upscale).
* A selection of display modes: vertical strip, dual page
* Not much more, anything I don't personally use doesn't get implemented.

# Installation

`JEMALLOC_SYS_WITH_MALLOC_CONF="background_thread:true,oversize_threshold:0" cargo install --git https://github.com/awused/aw-man --locked`

`JEMALLOC_SYS_WITH_MALLOC_CONF` is used to tweak jemalloc for greater performance with large allocations.

Copy [aw-man.toml.sample](aw-man.toml.sample) to `~/.config/aw-man/aw-man.toml` or `~/.aw-man.toml` and fill it out according to the instructions.

If you have trouble getting upscaling to work, make sure that waifu2x-ncnn-vulkan is on your PATH. The directory containing the waifu2x-ncnn-vulkan binary should also contain the [models-cunet](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models/models-cunet) directory.

Additional optional files for installation can be found in the [desktop](desktop) directory.

# Dependencies

Required:

* GTK - GTK4 libraries and development headers must be installed.
    * Pixbuf is used as a fallback to support a wider variety of formats.
* libarchive - Used to extract images from archive files.
* libwebp
* libjxl
* opengl

On fedora all required dependencies can be installed with `dnf install gtk4-devel libarchive-devel libwebp-devel jpegxl-devel`.


Optional:

* unrar - Support reading from rar files that aren't supported by libarchive is provided by using the unrar _binary_.
    * unrar is disabled by default and must be enabled in the config.
* Additional Pixbuf loader plugins.
    * These can add support for less common formats, like avif and heif.

Upscaling has additional default requirements, but can be configured to use others:

* [waifu2x-ncnn-vulkan](https://github.com/nihui/waifu2x-ncnn-vulkan) When installing waifu2x, make sure that the [models](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models) directory is present (copied or symlinked) in the same directory as the executable.
* [PyGObject](https://pygobject.readthedocs.io/) is also preferred by the default upscaler.
    * [ImageMagick 6 or 7](https://imagemagick.org/script/download.php) will be used as a fallback if PyGobject is not available.

Alternative upscalers can be configured in place of waifu2x-ncnn-vulkan, see [aw-upscale](https://github.com/awused/aw-upscale).

# Usage

Run `aw-man archive-of-images.zip` or `aw-man image.png` and view the images. Also works non-recursively on directories of images. Push `U` to switch to viewing an upscaled version of the images.

The manga mode (`-manga`, `-m` or the `M` shortcut) causes it to treat the directory containing the archive as it if contains a series of volumes or chapters of manga. The next chapter or volume should follow after the last page of the current archive. Supports the directory structure produced by [manga-syncer](https://github.com/awused/manga-syncer) but should work with any archives that sort sensibly.

# Shortcuts

Default Shortcut | Action
-----------------|-----------
`Down Arrow/Mouse Wheel Down` | Scrolls down, possibly to the next page.
`Up Arrow/Mouse Wheel Up` | Scrolls up, possibly to the previous page.
`Right Arrow` | Scrolls right.
`Left Arrow` | Scrolls left.
`Page Down` | Moves to the next page.
`Page Up` | Moves to the previous page.
`Home/End` | Moves to the First/Last page in the current archive.
`]` | Moves to the next archive in the same directory.
`[` | Moves to the previous archive in the same direcotry.
`H` | Hide the UI.
`B` | Pick a background colour.
`F` | Toggle fullscreen mode.
`U` | Toggle upscaling.
`M` | Toggle manga mode, enabling continuous scrolling through chapters in the same directory.
`Space` | Pause or play the current animation or video.
`J` | Jump to a specific page, either in absolute or relative (+/-) terms.
`Q/Esc` | Quit.
`Alt+F` | Display images at their full size, scrolling if necessary.
`Alt+W` | Fit images to the width of the window, scrolling vertically if necessary.
`Alt+H` | Fit images to the height of the window, scrolling horizontally if necessary.
`Alt+C` | Fit images inside the window. Images will not need to scroll.
`Alt+S` | Single page display mode.
`Alt+V` | Vertical strip display mode. Display multiple images at once to fill the screen vertically.
`Alt+O` | Horizontal strip display mode. Display multiple images at once to fill the screen horizontally.
`Alt+D` | Dual page mode. Display two pages side-by-side.
`Alt+R` | Reversed dual page mode. Display two pages side-by-side, with the first to the right of the second.

## Customization

Keyboard shortcuts and context menu entries can be customized in [aw-man.toml](aw-man.toml.sample). See the comments in the config file for how to specify them.

Recognized internal commands:

* NextPage/PreviousPage
* ScrollDown/ScrollUp
    * These may switch to the next or previous page.
* ScrollRight/ScrollLeft
* FitToContainer/FitToWidth/FitToHeight/FullSize
* SinglePage/VerticalStrip/HorizontalStrip/DualPage/DualPageReversed
  * Change how pages are displayed.
* FirstPage/LastPage
* NextArchive/PreviousArchive
* Quit
* ToggleUI
* SetBackground
    * Spawns a dialog allowing the user to select a new background colour.
    * Optionally takes a string recognized by GDK as a colour.
    * Examples: `SetBackground #aaaaaa55` `SetBackground magenta`
* ToggleFullscreen
* ToggleMangaMode
* ToggleUpscaling
* TogglePlaying
* Jump
  * Spawns a dialog allowing the user to enter the number of the page they want to display, or the number of pages to shift.
  * Optionally takes an integer argument as either an absolute jump within the same chapter or a relative jump, which can span multiple chapters in Manga mode.
  * Absolute jumps are one-indexed.
  * Examples: `Jump 25`, `Jump +10`, `Jump -5`
* Execute
  * Requires a single string argument which will be run as an executable.
  * Example: `Execute /path/to/save-page.sh`

## External Executables

Using the "Execute" action you can run any arbitrary executable. That executable will be called with no arguments and several environment variables set. [save-page.sh](examples/save-page.sh) is an example that implements the common save page as file action.

Environment Variable | Explanation
-------------------- | ----------
AWMAN_ARCHIVE | The path to the current archive or directory that is open.
AWMAN_ARCHIVE_TYPE | The type of the archive, one of `archive`, `directory`, `fileset`, or `unknown`.
AWMAN_RELATIVE_FILE_PATH | The path of the current file relative to the root of the archive or directory.
AWMAN_PAGE_NUMBER | The page number of the currently open file.
AWMAN_CURRENT_FILE | The path to the extracted file or, in the case of directories, the original file. It should not be modified or deleted.
AWMAN_PID | The PID of the aw-man process.
AWMAN_WINDOW | The window ID for the primary window. Currently only on X11.
AWMAN_SOCKET | The socket used for IPC, if enabled.
AWMAN_DISPLAY_MODE | The current display mode, either `single` or `verticalstrip`.
AWMAN_FIT_MODE | The current fit mode, one of `container`, `height`, `width`, or `verticalstrip`.
AWMAN_UPSCALING_ENABLED | Whether upscaling is enabled or not.
AWMAN_MANGA_MODE | Whether manga mode is enabled or not.

# Scripting

If configured, aw-man will expose a limited API over a unix socket, one per process. See the documentation in [aw-man.toml](aw-man.toml.sample) and the [example script](examples/socket-print.sh).

Request | Response
--------|---------------------------------------------------------------------------------------
Status  | The same set of environment variables sent to shortcut executables.
ListPages  | List the pages in the current archive.

The API also accepts any valid action that you could specify in a shortcut, including external executables. Don't run this as root.

# Why

I wrote [manga-upscaler](https://github.com/awused/manga-upscaler) for use with mangadex's web viewer but now have a need for something more controllable. Most of the complexity of an image viewer or comic book reader comes from all the customization offered and aw-man has little of that. This program is very much written to fit my needs and little more, which is roughly an mcomix-like image viewer that is much faster.

There is also a perfectly functional version in the cpu branch that does not depend on opengl.
