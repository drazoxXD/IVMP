-- Client side of the example resource. It runs inside the game, after the server sent it.
-- (Natives, UI and the player come in later stages; for now: events, threads, commands, JSON.)

print('example client script started')

-- The server answers `triggerserver ping 5` (F1 console) with this event.
RegisterNetEvent('pong', function(n)
    print('the server answered: ' .. tostring(n))
    SendNUIMessage({ type = 'pong', n = n }) -- the page shows it
end)

-- Type `hello` into the F1 console.
RegisterCommand('hello', function(source, args)
    print('Hello from a client script, ' .. (args[1] or 'world') .. '!')
    TriggerServerEvent('ping', 41)
end)

CreateThread(function()
    local seconds = 0
    while true do
        Wait(60000)
        seconds = seconds + 60
        print('client script alive for ' .. seconds .. ' s')
    end
end)

-- Natives are global functions, like in FiveM, in any style of their name (GET_PLAYER_CHAR, GetPlayerChar,
-- getPlayerChar): about 3000 are known with their argument types. `where` in the F1 console prints
-- where you are. A native that writes into pointers returns those values (the ped, the coordinates).
RegisterCommand('where', function()
    local ped = GetPlayerChar(0)
    local x, y, z = GetCharCoordinates(ped)
    print(string.format('you are at %.1f, %.1f, %.1f (ped %d)', x, y, z, ped))
end)

-- The page (ui/index.html) calls this with ivmpNui.post('ping', { n = 1 }); cb answers the page.
RegisterNUICallback('ping', function(data, cb)
    TriggerServerEvent('ping', data.n)
    cb({ ok = true })
end)

-- `ui` in the F1 console: the page gets the mouse and keyboard (Escape gives them back).
RegisterCommand('ui', function()
    SetNuiFocus(true, true)
end)

-- Sounds (wav, mp3, ogg, flac files of the resource) at a place in the world, muffled by what is in
-- between (interiors for now): `beep` puts a looping beep 15 units ahead of the player, `nobeep` stops it.
local beep
RegisterCommand('beep', function()
    local ped = GetPlayerChar(0)
    local x, y, z = GetCharCoordinates(ped)
    if beep then StopSound(beep) end
    beep = PlaySoundAtCoords('sounds/beep.wav', x + 15.0, y, z, { volume = 1.0, radius = 60.0, loop = true })
    print('beep at ' .. string.format('%.1f %.1f %.1f', x + 15.0, y, z) .. ' (sound ' .. beep .. ')')
end)
RegisterCommand('nobeep', function()
    if beep then StopSound(beep) beep = nil end
end)
