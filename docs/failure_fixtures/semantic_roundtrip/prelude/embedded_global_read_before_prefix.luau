-- Loading another source chunk makes its shared environment unsafe.
getfenv()
function setG(v) G = v end
