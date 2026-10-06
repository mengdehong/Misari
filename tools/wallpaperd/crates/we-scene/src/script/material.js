'use strict';
// Material properties use the same tracked values and animation paths as property scripts.
function __wePrepareMaterial(value,path) {
    if(path.at(-1)!=='constantshadervalues'||Array.isArray(value))return;
    for(const [key,raw] of Object.entries(value)) {
        const values=typeof raw==='string'?raw.trim().split(/\s+/).map(Number):raw;
        if(Array.isArray(values)&&values.length>=2&&values.length<=4&&values.every(Number.isFinite))value[key]=__weVector(values,values.length);
    }
}
function __weMaterialAt(index,path,slot) {
    __weCheck(index);
    if(!Number.isInteger(slot)||slot<0)throw new RangeError('Invalid material index');
    const passes=__wePath(__weNodes[index],path);
    return passes?.[slot]?.constantshadervalues;
}
function __weInstallEffect(value,index,path) {
    if(path.length!==2||path[0]!=='effects'||!value||typeof value!=='object'||Object.hasOwn(value,'getMaterial'))return;
    const slot=path[1],passes=path.concat('passes');
    Object.defineProperties(value,{
        getMaterial:{value:material=>__weMaterialAt(index,passes,material)},
        getMaterialCount:{value:()=>{__weCheck(index);return __weNodes[index].effects[slot].passes.length;}},
        setMaterialProperty:{value:(name,v)=>{
            __weCheck(index);
            if(typeof name!=='string'||!(typeof v==='number'&&Number.isFinite(v)||v instanceof WEVec&&v.isFinite()))throw new TypeError('Invalid material property');
            for(const pass of __weNodes[index].effects[slot].passes)if(Object.hasOwn(pass.constantshadervalues,name))pass.constantshadervalues[name]=v instanceof WEVec?v.copy():v;
        }},
    });
}
function __weInstallMaterials(raw,index) {
    if(raw.image===undefined&&raw.text===undefined&&raw.__materials===undefined)return;
    Object.defineProperties(raw,{
        getEffect:{value:key=>{__weCheck(index);const effects=__weNodes[index].effects??[];return typeof key==='number'?effects[key]:effects.find(e=>e.name===key);}},
        getEffectCount:{value:()=>{__weCheck(index);return (__weNodes[index].effects??[]).length;}},
    });
}
