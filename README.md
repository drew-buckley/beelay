# beelay
This program is a simple webserver that provides a HTTP API for control of Zigbee smart switches. I made this to control my SONOFF from a Raspberry Pi and as an excuse to get some Rust learning in. Its pretty hacky at the moment, but it does work fairly reliably. The one exception to this is the `GET`s are kind of broken... They "work", but they take a couple seconds to reply. If I get some time sometime soon, I'll hopefully clean this up and fix the `GET`s. 

Here's some examples of its use:
```
~$ curl  --request POST  "pi.local:9999/?switch=Bedroom%20TV&state=OFF" ; echo
{"status":"success"}
```

```
~$ curl  --request GET "pi.local:9999/?switch=Bedroom%20TV" ; echo
{"status":"success","switch_state":"OFF"}
```

```
~$ curl  --request POST  "pi.local:9999/?switch=Bedroom%20TV&state=ON" ; echo
{"status":"success"}
```

```
~$ curl  --request GET "pi.local:9999/?switch=Bedroom%20TV" ; echo
{"status":"success","switch_state":"ON"}
```

Example of an error:
```
~$ curl  --request GET "pi.local:9999/?switch=Not%20a%20real%20device" ; echo
{"error_msg":"Unknown switch name.","status":"error"}
```
