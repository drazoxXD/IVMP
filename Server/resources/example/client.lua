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

-- Natives by name (about 3000 are known, with their argument types): `where` in the F1 console
-- prints where you are. Out parameters (the ped, the coordinates) come back as return values.
RegisterCommand('where', function()
    local ped = Natives.GET_PLAYER_CHAR(0)
    local x, y, z = Natives.GET_CHAR_COORDINATES(ped)
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
