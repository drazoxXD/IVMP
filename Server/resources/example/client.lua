-- Client side of the example resource. It runs inside the game, after the server sent it.
-- (Natives, UI and the player come in later stages; for now: events, threads, commands, JSON.)

print('example client script started')

-- The server answers `triggerserver ping 5` (F1 console) with this event.
RegisterNetEvent('pong', function(n)
    print('the server answered: ' .. tostring(n))
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
