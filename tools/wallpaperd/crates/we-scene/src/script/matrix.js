'use strict';
class WEMatrix {
    constructor(n) {Object.defineProperty(this,'_n',{value:n});this.m=Array.from({length:n*n},(_,i)=>i%n===Math.floor(i/n)?1:0);}
    _array(m) {const out=new this.constructor();out.m=m;return out;}
    copy() {return this._array(this.m.slice());}
    equals(v) {return v instanceof WEMatrix && this._n===v._n && this.m.every((x,i)=>Math.abs(x-v.m[i])<1e-5);}
    add(v) {return this._array(this.m.map((x,i)=>x+v.m[i]));}
    subtract(v) {return this._array(this.m.map((x,i)=>x-v.m[i]));}
    multiply(v) {
        const n=this._n;
        if(typeof v==='number')return this._array(this.m.map(x=>x*v));
        if(v instanceof WEVec && v._n===n) {const a=v.toArray();return new v.constructor(...Array.from({length:n},(_,r)=>a.reduce((sum,x,c)=>sum+x*this.m[c*n+r],0)));}
        if(!(v instanceof WEMatrix)||v._n!==n)throw new TypeError('Matrix dimension mismatch');
        return this._array(Array.from({length:n*n},(_,i)=>{const c=Math.floor(i/n),r=i%n;let s=0;for(let k=0;k<n;k++)s+=this.m[k*n+r]*v.m[c*n+k];return s;}));
    }
    transpose() {const n=this._n;return this._array(this.m.map((_,i)=>this.m[(i%n)*n+Math.floor(i/n)]));}
    determinant() {return __weMatrix('determinant',this._n,this.m)[0];}
    inverse() {return this._array(__weMatrix('inverse',this._n,this.m));}
    toString() {return this.m.join(' ');}
    toJSON() {return this.m;}
}
class Mat4 extends WEMatrix {
    constructor() {super(4);}
    static identity() {return new Mat4();}
    static fromTranslation(v) {const m=new Mat4();m.translation(v);return m;}
    static fromScale(v) {if(typeof v==='number')v=new Vec3(v);const m=new Mat4();m.m[0]=v.x;m.m[5]=v.y;m.m[10]=v.z;return m;}
    static fromRotation(a,axis) {return new Mat4()._array(__weMatrix('rotation',4,[a,axis.x,axis.y,axis.z]));}
    static fromEuler(x,y,z) {const v=x instanceof Vec3?x:new Vec3(x??0,y??0,z??0);return new Mat4()._array(__weMatrix('euler',4,v.toArray()));}
    static fromBasis(right,up,forward) {const m=new Mat4();for(let i=0;i<3;i++){m.m[i]=right.toArray()[i];m.m[4+i]=up.toArray()[i];m.m[8+i]=forward.toArray()[i];}return m;}
    static lookAt(eye,center,up) {return new Mat4()._array(__weMatrix('lookAt',4,[...eye.toArray(),...center.toArray(),...up.toArray()]));}
    static compose(t,r,s) {return Mat4.fromTranslation(t).multiply(Mat4.fromEuler(r)).multiply(Mat4.fromScale(s));}
    translation(v) {if(v!==undefined){this.m[12]=v.x;this.m[13]=v.y;this.m[14]=v.z??0;return;}return new Vec3(...this.m.slice(12,15));}
    right() {return new Vec3(...this.m.slice(0,3));}
    up() {return new Vec3(...this.m.slice(4,7));}
    forward() {return new Vec3(...this.m.slice(8,11));}
    translate(v) {return this.multiply(Mat4.fromTranslation(v));}
    rotate(a,axis) {return this.multiply(Mat4.fromRotation(a,axis));}
    scale(v) {return this.multiply(Mat4.fromScale(v));}
    transformPoint(v) {const p=this.multiply(new Vec4(v,1));return new Vec3(p);}
    transformDirection(v) {return new Vec3(this.multiply(new Vec4(v,0)));}
    normalMatrix() {return Mat3.fromMat4(this).inverse().transpose();}
    decompose() {const a=__weMatrix('decompose',4,this.m);return {translation:new Vec3(...a.slice(0,3)),rotation:new Vec3(...a.slice(3,6)),scale:new Vec3(...a.slice(6,9))};}
    extractEuler() {return this.decompose().rotation;}
}
class Mat3 extends WEMatrix {
    constructor() {super(3);}
    static identity() {return new Mat3();}
    static fromTranslation(v) {const m=new Mat3();m.translation(v);return m;}
    static fromScale(v) {if(typeof v==='number')v=new Vec2(v);const m=new Mat3();m.m[0]=v.x;m.m[4]=v.y;return m;}
    static fromRotation(a) {const c=Math.cos(a*Math.PI/180),s=Math.sin(a*Math.PI/180);return new Mat3()._array([c,s,0,-s,c,0,0,0,1]);}
    static fromBasis(right,up) {return new Mat3()._array([right.x,right.y,0,up.x,up.y,0,0,0,1]);}
    static fromMat4(v) {return new Mat3()._array([v.m[0],v.m[1],v.m[2],v.m[4],v.m[5],v.m[6],v.m[8],v.m[9],v.m[10]]);}
    static compose(t,r,s) {return Mat3.fromTranslation(t).multiply(Mat3.fromRotation(r)).multiply(Mat3.fromScale(s));}
    translation(v) {if(v!==undefined){this.m[6]=v.x;this.m[7]=v.y;return;}return new Vec2(this.m[6],this.m[7]);}
    angle() {return Math.atan2(this.m[1],this.m[0])*180/Math.PI;}
    translate(v) {return this.multiply(Mat3.fromTranslation(v));}
    rotate(a) {return this.multiply(Mat3.fromRotation(a));}
    scale(v) {return this.multiply(Mat3.fromScale(v));}
    transformPoint(v) {return new Vec2(this.multiply(new Vec3(v,1)));}
    transformDirection(v) {return new Vec2(this.multiply(new Vec3(v,0)));}
    decompose() {let sx=Math.hypot(this.m[0],this.m[1]),sy=Math.hypot(this.m[3],this.m[4]);if(this.determinant()<0)sx=-sx;return {translation:this.translation(),rotation:Math.atan2(this.m[1]/sx,this.m[0]/sx)*180/Math.PI,scale:new Vec2(sx,sy)};}
}
function __weMatrixBinary(op,a,b) {
    if(op==='*')return a instanceof WEMatrix?a.multiply(b):b.multiply(a);
    if(op==='/' && a instanceof WEMatrix && typeof b==='number')return a.multiply(1/b);
    if(a instanceof WEMatrix && b instanceof WEMatrix && a._n===b._n){if(op==='+')return a.add(b);if(op==='-')return a.subtract(b);}
    throw new TypeError('Unsupported matrix operator');
}
