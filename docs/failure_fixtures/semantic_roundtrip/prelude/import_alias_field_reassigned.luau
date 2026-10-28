-- Loading another source chunk makes its shared environment unsafe.
getfenv()
Shared = { fn = function() return 'old' end }
