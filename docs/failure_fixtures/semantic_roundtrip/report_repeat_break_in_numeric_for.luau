local _checksum = 0                                                                                                                                 
  local _call_limit = 150                                                                                                                             
  local function _hash(val, type_id)                                                                                                                  
      if type_id == 1 then                                                                                                                            
          _checksum = (_checksum + val) % 2147483647                                                                                                  
      elseif type_id == 2 then                                                                                                                        
          _checksum = (_checksum + (val and 1 or 0)) % 2147483647                                                                                     
      end                                                                                                                                             
      return val                                                                                                                                      
  end                                                                                                                                                 
                                                                                                                                                      
  if (not ({} and (not (_hash(("uEFg"):len(), 1) or bit32.band(_hash(-8, 1)))))) then                                                                 
      v1 = 32                                                                                                                                         
  end                                                                                                                                                 
  if (function()                                                                                                                                      
      _call_limit = _call_limit - 1                                                                                                                   
      if _call_limit < 0 then return false end                                                                                                        
      return false                                                                                                                                    
  end)() then                                                                                                                                         
      for v2 = 1, 1 do                                                                                                                                
          repeat                                                                                                                                      
              _loop_5 = _loop_5 + 1                                                                                                                   
              if _loop_5 > 5 then break end                                                                                                           
              v3 = v4                                                                                                                                 
          until {}                                                                                                                                    
          break                                                                                                                                       
      end                                                                                                                                             
  end                                                                                                                                                 
  if _hash({}, 2) then                                                                                                                                
  end                                                                                                                                                 
                                                                                                                                                      
  print('CHECKSUM:', _checksum)  