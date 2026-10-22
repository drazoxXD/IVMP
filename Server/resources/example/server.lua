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
