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

//! **Nirius** is a window-switcher and more for the niri window manager.
//! It consists of a daemon, and a client.  The `nirius` client offers
//! subcommands, see `nirius --help` and sends them to the daemon `niriusd`
//! which executes them.

pub mod client;
pub mod cmds;
pub mod daemon;
pub mod ipc;
pub mod state;
pub mod util;
