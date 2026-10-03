// Copyright (C) 2025  Tassilo Horn <tsdh@gnu.org>
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License for
// more details.
//
// You should have received a copy of the GNU General Public License along with
// this program.  If not, see <https://www.gnu.org/licenses/>.

pub fn get_nirius_socket_path() -> String {
    // TODO: Is the comment below still accurrate?  I think so.  At least in
    // directories 6.0.0 it is still checked that the runtime dir is absolute.
    // That might be dictated by the spec but it's not really important for
    // us...
    //
    // We prefer checking the env variable instead of
    // directories::BaseDirs::new().unwrap().runtime_dir().unwrap() because
    // directories errors if the XDG_RUNTIME_DIR isn't set or set to a relative
    // path which actually works fine for us.
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR");
    let wayland_display = std::env::var("WAYLAND_DISPLAY");
    format!(
        "{}/nirius-{}.sock",
        match xdg_runtime_dir {
            Ok(val) => val,
            Err(_e) => {
                log::error!("Couldn't get XDG_RUNTIME_DIR!");
                String::from("/tmp")
            }
        },
        match wayland_display {
            Ok(val) => val,
            Err(_e) => {
                log::error!("Couldn't get WAYLAND_DISPLAY!");
                String::from("unknown")
            }
        }
    )
}
