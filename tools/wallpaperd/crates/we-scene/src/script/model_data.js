'use strict';
const __weModelHandles=new WeakMap();
const __weModelToken={};
class IModelData {
    static POSITION='position';static NORMAL='normal';static TANGENT_SIGNED='tangentSigned';static UV='uv';static COLOR='color';
    constructor(id,token){if(token!==__weModelToken)throw new TypeError('Use thisScene.createModelData');__weModelHandles.set(this,id);Object.freeze(this);}
    applyData(shapes){__weModelData('apply',__weModelId(this),shapes);}
    replaceData(shapes){
        if(__weCurrentEvent==='update')throw new TypeError('ModelData.replaceData cannot be called in update');
        __weModelData('replace',__weModelId(this),shapes);
    }
    getAnimation(){__weModelId(this);return undefined;}
}
for(const key of ['POSITION','NORMAL','TANGENT_SIGNED','UV','COLOR'])Object.defineProperty(IModelData.prototype,key,{value:IModelData[key]});
function __weModelId(handle){
    const id=__weModelHandles.get(handle);
    if(id===undefined)throw new TypeError('Invalid ModelData handle');
    __weModelData('validate',id,null);return id;
}
