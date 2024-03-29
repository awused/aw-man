# Creates an "Open with aw-man" context menu item for common compressed formats.
param([switch]$Elevated)

function Test-Admin {
    $currentUser = New-Object Security.Principal.WindowsPrincipal $([Security.Principal.WindowsIdentity]::GetCurrent())
    $currentUser.IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
}

if ((Test-Admin) -eq $false)  {
    if ($elevated) {
        # tried to elevate, did not work, aborting
    } else {
        Start-Process powershell.exe -Verb RunAs -ArgumentList ('-noprofile -noexit -file "{0}" -elevated' -f ($myinvocation.MyCommand.Definition))
    }
    exit
}

$executable = (Get-Command aw-man -ErrorAction Stop).Path 

$command = """$executable"" ""%1"""

New-PSDrive -PSProvider registry -Root HKEY_CLASSES_ROOT -Name HKCR

$extensions = @(".zip", ".7z", ".rar", ".cbz", ".cbr")

ForEach ($ext in $extensions) {
	$path = "HKCR:\SystemFileAssociations\$ext\Shell\Open with aw-man\command"
	New-Item -Path $path -Force -Value $command -ErrorAction Stop
}

echo "Installed all context menu entries"