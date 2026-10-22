-- A small example resource: events, commands, threads, timers and an export.
ivmp_version 'beta'
author 'IVMP'
description 'Shows the server scripting API'
version '1.0.0'

server_scripts { 'server.lua' }
client_scripts { 'client.lua' }
files { 'ui/index.html' }
ui_page 'ui/index.html'
