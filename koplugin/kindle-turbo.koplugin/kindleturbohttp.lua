-- Native KOReader HTTP transport shared by source adapters.
local http = require("socket.http")
local socket = require("socket")
local socketutil = require("socketutil")
local Http = {}
local last_request = {}

function Http.get(url, site, interval, image)
    local now = socket.gettime()
    local remaining = (last_request[site] or 0) + interval - now
    if remaining > 0 then socket.sleep(remaining) end
    last_request[site] = socket.gettime()
    local chunks, size = {}, 0
    socketutil:set_timeout(socketutil.LARGE_BLOCK_TIMEOUT, socketutil.LARGE_TOTAL_TIMEOUT)
    local ok, code = pcall(function()
        return socket.skip(1, http.request{
            url = url,
            headers = {
                ["User-Agent"] = "Mozilla/5.0 Kindle-Turbo/0.1",
                ["Referer"] = site .. "/",
                ["Accept"] = image and "image/avif,image/webp,*/*" or "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ["Accept-Encoding"] = "identity",
            },
            sink = function(chunk)
                if chunk then
                    size = size + #chunk
                    if size > 16 * 1024 * 1024 then return nil, "Resposta maior que 16 MiB" end
                    chunks[#chunks + 1] = chunk
                end
                return 1
            end,
        })
    end)
    socketutil:reset_timeout()
    if not ok or code ~= 200 then return nil, "HTTP " .. tostring(code) .. ". Tente novamente mais tarde." end
    return table.concat(chunks)
end

function Http.imageExtension(data)
    if data:sub(1, 3) == "\255\216\255" then return "jpg" end
    if data:sub(1, 8) == "\137PNG\13\10\26\10" then return "png" end
    if data:sub(1, 4) == "RIFF" and data:sub(9, 12) == "WEBP" then return "webp" end
    if data:sub(1, 3) == "GIF" then return "gif" end
    return nil, "O servidor não retornou uma imagem válida"
end

return Http
