-- Server side of the example resource.

print('example started, ' .. GetNumPlayerIndices() .. ' player(s) online')

-- Players coming and going.
AddEventHandler('playerJoining', function()
    print(GetPlayerName(source) .. ' joined (id ' .. source .. ')')
end)

AddEventHandler('playerDropped', function(reason)
    print(GetPlayerName(source) .. ' left: ' .. reason)
end)

-- A command: type `hello` or `hello Niko` into the server console.
RegisterCommand('hello', function(source, args, raw)
    local who = args[1] or 'world'
    print('Hello, ' .. who .. '!')
end)

-- `players` lists everybody who is connected.
RegisterCommand('players', function()
    local players = GetPlayers()
    if #players == 0 then
        print('nobody is online')
    end
    for _, id in ipairs(players) do
        print(id .. ': ' .. GetPlayerName(id))
    end
end)

-- A thread that wakes up every 30 seconds.
CreateThread(function()
    while true do
        Wait(30000)
        print('still running: ' .. GetNumPlayerIndices() .. ' player(s) online, up ' .. GetGameTimer() // 1000 .. ' s')
    end
end)

-- A one-off timer.
SetTimeout(5000, function()
    print('five seconds after start')
end)

-- Other resources can call this: exports.example:playerCount()
exports('playerCount', function()
    return GetNumPlayerIndices()
end)

-- A client can send this with the console command `triggerserver ping 5`; the server answers with
-- a client event (the client console shows it as "script event 'pong' ...").
RegisterNetEvent('ping', function(n)
    print(GetPlayerName(source) .. ' pinged with ' .. tostring(n))
    TriggerClientEvent('pong', source, (n or 0) + 1)
end)

-- Refuse a join:
-- AddEventHandler('playerConnecting', function(name)
--     if name == 'Banned' then SetKickReason('you are banned') CancelEvent() end
-- end)

-- `/car <model>` in the chat (or `car <model>` in the F1 console): a vehicle next to the player. The
-- server owns it and a client creates it with the engine's factory, like the `spawn car` command.
-- GetPlayerCoords(source) is where the server last saw that player.
RegisterCommand('car', function(source, args)
    local x, y, z = GetPlayerCoords(source)
    if not x then
        print('no position known yet for player ' .. tostring(source))
        return
    end
    local model = args[1] or 'sultan'
    local vehicle = CreateVehicle(model, x + 5.0, y, z, 0.0)
    print(GetPlayerName(source) .. ' spawned a ' .. model .. ' (entity ' .. vehicle .. ')')
end)

-- DeleteEntity(entity) removes it again; CreatePed(model, x, y, z, heading) makes a ped.
