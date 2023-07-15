# beelay
This program is a simple webserver that provides a HTTP API for control of Zigbee smart switches. This is expected to be run alongside `zigbee2mqtt` and basically acts as a wrapper around it.

I made this to control my fleet of SONOFF Zigbee switches from a Raspberry Pi and as an excuse to get some Rust learning in. Its pretty hacky at the moment, but it does work fairly reliably (I've been exclusively using this for several switches, running the service for days and days at a time). The `GET`s now work too (albeit with a workaround), and the service will stay in sync with manual toggling of a switch via its physical button. 

No idea if this works with other Zigbee smart switches beyond the SONOFF ones I've been using.

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
