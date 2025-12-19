# AW-MAN

Awused's personal manga reader/image viewer.

It is a simple viewer with support for running arbitrary upscalers, like waifu2x.

# Features

* Fast, GPU accelerated, and memory efficient reader.
    * The priorities are quality, then latency, then vram usage, then memory usage, then CPU usage.
    * Animated gifs are moderately memory-inefficient.
* Correct gamma and alpha handling during scaling and presentation.
* Wide support for many archive and image formats.
* Proper natural sorting of chapters even with decimal chapter numbers.
    * Works well with [manga-syncer](https://github.com/awused/manga-syncer), but generally matches expected sorting order.
* Configurable shortcuts to run external scripts and a basic IPC interface.
* Support for custom external upscalers. See [aw-upscale](https://github.com/awused/aw-upscale).
* Good support for manga layouts including side-by-side pages and long strips.
* Not much more, anything I don't personally use doesn't get implemented.

# Usage

Run `aw-man archive-of-images.zip` or `aw-man image.png` and view the images. Also works non-recursively on directories of images. Push `U` to switch to viewing an upscaled version of the images. Dragging and dropping files works too.

Manga mode (`--manga`, `-m` or the `M` shortcut) causes it to treat the directory containing the archive as it if contains a series of volumes or chapters of manga. The next chapter or volume should follow after the last page of the current archive. Supports the directory structure produced by [manga-syncer](https://github.com/awused/manga-syncer) but should work with any archives that sort sensibly.

See `aw-man -h` for more usage information.

It's Recommended to copy [aw-man.toml.sample](aw-man.toml.sample) to `~/.config/aw-man/aw-man.toml` or `~/.aw-man.toml` and fill it out according to the instructions in the file.

# Installation

`JEMALLOC_SYS_WITH_MALLOC_CONF="background_thread:true,oversize_threshold:0" cargo install --git https://github.com/awused/aw-man --locked`

`JEMALLOC_SYS_WITH_MALLOC_CONF` is used to tweak jemalloc for greater performance with large allocations. This doesn't apply on Windows. It should be automatically set but better to set it explicitly.


If you have trouble getting upscaling to work, make sure that waifu2x-ncnn-vulkan is on your PATH. The directory containing the waifu2x-ncnn-vulkan binary should also contain the [models-cunet](https://github.com/nihui/waifu2x-ncnn-vulkan/tree/master/models/models-cunet) directory.

Additional optional files for installation can be found in the [desktop](desktop) directory.

# Dependencies

Default:

* GTK - GTK4 libraries and development headers must be installed.
    * Pixbuf is used as a fallback to support a wider variety of formats.
* libarchive - Used to extract images from archive files.
* libjxl
* opencl
  * This can optionally be disabled with `--default-features false`
* opengl
* libepoxy
    * Should already be present on most Linux and Windows systems, but may need to be installed on Mac.

On Fedora all default dependencies can be installed with `dnf install gtk4-devel libarchive-devel jpegxl-devel ocl-icd-devel`.


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

# Shortcuts

Default Shortcut | Action
-----------------|-----------
`?` | List current keybinds.
`Down Arrow/Mouse Wheel Down` | Scrolls down, possibly to the next page.
`Up Arrow/Mouse Wheel Up` | Scrolls up, possibly to the previous page.
`Right Arrow/Left Arrow` | Scrolls right or left. Cannot change the current page except in horizontal strip mode.
`Shift+Arrow Keys` | Snaps to the top, bottom, left, or right side of the current page.
`Page Down` | Moves to the next page.
`Page Up` | Moves to the previous page.
`Ctrl+Page Down/Page Up` | Move to the next or previous page without changing the scroll position.
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
`Shift+Q/Shift+Esc` | Quit, but do not run any configured quit commands.
`Alt+F` | Display images at their full size, scrolling if necessary.
`Alt+W` | Fit images to the width of the window, scrolling vertically if necessary.
`Alt+H` | Fit images to the height of the window, scrolling horizontally if necessary.
`Alt+C` | Fit images inside the window. Images will not need to scroll.
`Alt+S` | Single page display mode.
`Alt+V` | Vertical strip display mode. Display multiple images at once to fill the screen vertically.
`Alt+O` | Horizontal strip display mode. Display multiple images at once to fill the screen horizontally.
`Alt+D` | Dual page mode. Display two pages side-by-side.
`Alt+R` | Reversed dual page mode. Display two pages side-by-side, with the first to the right of the second.
`Ctrl+C` | Copy the path of the current page (may not be the only visible page) into the clipboard. This may be an extracted file.
`Ctrl+O` | Open new files. For both standalone pages and compressed archives.
`Ctrl+Shift+O` | Open a new directory.

## Customization

Keyboard shortcuts and context menu entries can be customized in [aw-man.toml](aw-man.toml.sample). See the comments in the config file for how to specify them.

Recognized internal commands:

* `Help`
  * List current keybinds.
* `NextPage`/`PreviousPage`/`FirstPage`/`LastPage`
  * Optionally takes an argument of `start`, `end`, or `current` to determine what portion of the page will be visible.
* `ScrollDown`/`ScrollUp`
  * These may switch to the next or previous page outside of strip mode.
  * Optionally takes a scroll amount as a positive integer `ScrollDown 500`
* `ScrollRight`/`ScrollLeft`
  * Optionally takes a scroll amount as a positive integer `ScrollRight 500`
* `SnapTop`/`SnapBottom`/`SnapLeft`/`SnapRight`
  * Snaps the screen so that the edges of the current page are visible.
* `FitToContainer`/`FitToWidth`/`FitToHeight`/`FullSize`
* `SinglePage`/`VerticalStrip`/`HorizontalStrip`/`DualPage`/`DualPageReversed`
  * Change how pages are displayed.
* `NextArchive`/`PreviousArchive`
* `Quit`
  * Pass in `nocommand` to avoid running any configured quit commands.
* `SetBackground`
  * Spawns a dialog allowing the user to select a new background colour.
  * Optionally takes a string recognized by GDK as a colour.
  * Examples: `SetBackground #aaaaaa55` `SetBackground magenta`
* `Fullscreen`/`MangaMode`/`Upscaling`/`Playing`/`UI`
  * Toggle the status of various modes.
    * `Fullscreen` - If the application is full screen.
    * `MangaMode` - If scrolling down from the last image in an archive will automaticlly load the next archive.
    * `Upscaling` - Whether or not external upscalers are in use.
    * `Playing` - Set whether animations and videos are playing.
    * `UI` - Hide or show the visible portions of the UI.
  * These optionally take an argument of `toggle`, `on` or `off`
  * Examples: `Fullscreen` (equivalent to `Fullscreen toggle`), `MangaMode on`, or `Playing off`
  * ToggleFullscreen/ToggleMangaMode/ToggleUpscaling/TogglePlaying/ToggleUI are older, deprecated versions that do not take arguments.
* `Jump`
  * Spawns a dialog allowing the user to enter the number of the page they want to display, or the number of pages to shift.
  * Optionally takes an integer argument as either an absolute jump within the same chapter or a relative jump, which can span multiple chapters in Manga mode.
  * Optionally takes a second argument of `start`, `end`, or `current` to determine what portion of the page will be visible.
  * Absolute jumps are one-indexed.
  * Examples: `Jump 25`, `Jump +10`, `Jump -5`, `Jump -4 start`, `Jump +1 current`
* `Execute`
  * Requires a single string argument which will be run as an executable.
  * Example: `Execute /path/to/save-page.sh`
* `Script`
  * Like Execute but reads stdout from the executable as a series of commands to run, one per line.
  * Waits for the script to finish. Will be killed on program exit.
    * Use `Execute` and the unix socket for more interactive scripting.
  * Example: `Script /path/to/sample-script.sh`
* `Open`/`OpenFolder`
  * Spawns a dialog allowing the user to open new files or a new folder.
  * Open can take a series of unescaped but quoted paths.
  * Example `Open /first/path/file.jpg /second/path/file2.jpg "/path/with spaces/file3.jpg"`
* `Copy`
  * Copy the path of the current file into the clipboard. There may be more than one page visible, but only one path will be copied.
  * Does not copy the contents of the page.
  * This will either be the original file or the path of a temporary file extracted from an archive.

## External Executables

Using the "Execute" action you can run any arbitrary executable. That executable will be called with no arguments and several environment variables set. [save-page.sh](examples/save-page.sh) is an example that implements the common save page as file action. Long-lived processes should prefer using the `Status` command for current values.

Environment Variable | Explanation
-------------------- | ----------
AWMAN_ANIMATION_PLAYING | Whether or not animations and videos are currently playing.
AWMAN_ARCHIVE | The path to the current archive or directory that is open.
AWMAN_ARCHIVE_TYPE | The type of the archive, one of `archive`, `directory`, `fileset`, or `unknown`.
AWMAN_BACKGROUND | The current background colour in `rgb(int, int, int)` or `rgba(int, int, int, float)` form.
AWMAN_CURRENT_FILE | The path to the extracted file or, in the case of directories, the original file. It should not be modified or deleted.
AWMAN_DISPLAY_MODE | The current display mode, one of `single`, `verticalstrip`, `horizontalstrip`, `dualpage`, or `dualpagereversed`.
AWMAN_FIT_MODE | The current fit mode, one of `container`, `height`, `width`, or `fullsize`.
AWMAN_FULLSCREEN | Wether or not the window is currently fullscreen.
AWMAN_MANGA_MODE | Whether manga mode is enabled or not.
AWMAN_PAGE_NUMBER | The page number of the currently open file.
AWMAN_PAGE_COUNT | The total number of pages in the current archive.
AWMAN_PID | The PID of the aw-man process.
AWMAN_RELATIVE_FILE_PATH | The path of the current file relative to the root of the archive or directory.
AWMAN_SOCKET | The socket used for IPC, if enabled.
AWMAN_UI_VISIBLE | Whether the UI (bottom bar) is currently visible.
AWMAN_UPSCALING_ENABLED | Whether upscaling is enabled or not.
AWMAN_WINDOW | The window ID for the primary window. Currently only on X11.

## Startup Commands and Lifecycle Hooks

An initial command can be sent to aw-man on startup with `--command "InternalCommand"`. This can be repeated for multiple commands and they will run in order.

A few other hooks are provided in the config to configure commands to run automatically: `startup_command`, `page_change_commadn`, `archive_change_command`, `idle_command`, `unidle_command`, `mode_change_command`, and `quit_command`. These were originally intended to allow for session saving and restoration. When both are present, `startup_command` runs after commands from cli arguments.

# External Scripting

If configured, aw-man will expose a limited API over a unix socket, one per process. See the documentation in [aw-man.toml](aw-man.toml.sample) and the [example script](examples/socket-print.sh).

Request | Response
--------|---------------------------------------------------------------------------------------
Status  | The same set of environment variables sent to shortcut executables.
ListPages  | List the pages in the current archive.

The API also accepts any valid action that you could specify in a shortcut, including external executables. Don't run this as root.

# Building on Windows

This isn't really recommended. GTK support for Windows is pretty sub-par and I haven't put much time into making it easy to build.

Assumes `vcpkg` and a Rust toolchain are already installed and `VCPKG_ROOT` is properly set. Install dependencies with `vcpkg install libarchive:x64-windows gtk:x64-windows libjxl:x64-windows libarchive:x64-windows-static-md dav1d:x64-windows`

Add `%VCPKG_ROOT%\installed\x64-windows\bin` to your `PATH`, without this you'll need to copy the DLLs produced elsewhere yourself.

For build make sure `%VCPKG_ROOT%\installed\x64-windows\lib\pkgconfig` is added to `PKG_CONFIG_PATH` and `%VCPKG_ROOT%\downloads\tools\msys2\9a1ec3f33446b195\mingw32\bin` is added to `PATH`. The string `9a1ec3f33446b195` may be out of date and might need to be updated to whatever is produced.

cmd.exe:
```bat
set PKG_CONFIG_PATH=%VCPKG_ROOT%\installed\x64-windows\lib\pkgconfig;%PKG_CONFIG_PATH%
set PATH=%VCPKG_ROOT%\downloads\tools\msys2\9a1ec3f33446b195\mingw32\bin;%PATH%
cargo install --git https://github.com/awused/aw-man --locked
```

powershell:
```PowerShell
$Env:Path += ";$Env:VCPKG_ROOT\downloads\tools\msys2\9a1ec3f33446b195\mingw32\bin"
$Env:PKG_CONFIG_PATH += ";$Env:VCPKG_ROOT\installed\x64-windows\lib\pkgconfig"
cargo install --git https://github.com/awused/aw-man --locked
```

Add `--features windows-console` to get console I/O, though this will spawn a console window when it's opened.

Assuming the cargo install path is already in your `PATH` then `aw-man some_file` should work. You can use the `GTK_THEME` environment variable to configure the theme, `Adwaita-dark` will switch to the bundled dark theme. Associate file types manually and use [OpenWithCompressed.ps1](desktop/OpenWithCompressed.ps1) to set up context menu entries for common archive formats.


I've also run into issues with Nvidia, gsync/freesync, and low idle clocks. If scrolling performance is choppy changing power settings to "prefer maximum performance" though this is unlikely to be a problem for normal use.

# Why

I wrote [manga-upscaler](https://github.com/awused/manga-upscaler) for use with mangadex's web viewer but now have a need for something more controllable. Most of the complexity of an image viewer or comic book reader comes from all the customization offered and aw-man has little of that. This program is very much written to fit my needs and little more, which is roughly an mcomix-like image viewer that is much faster.

There is also a perfectly functional version in the cpu branch that does not depend on opengl.
