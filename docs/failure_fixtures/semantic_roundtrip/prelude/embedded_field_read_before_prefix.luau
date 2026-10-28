-- Loading another source chunk makes its shared environment unsafe.
getfenv()
Shared = { x = 5 }
